// mpeg2dec.c: see mpeg2dec.h. Section numbers refer to ISO/IEC 13818-2.
#include "mpeg2dec.h"
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

// scan 0 (zigzag, Figure 7-2): scan position -> raster position
static const uint8_t zigzag[64] = {
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63 };
static const uint8_t default_intra[64] = {   // 6.3.11, raster order
    8, 16, 19, 22, 26, 27, 29, 34, 16, 16, 22, 24, 27, 29, 34, 37, 19, 22, 26, 27, 29, 34, 34, 38, 22, 22, 26, 27, 29, 34, 37, 40,
    22, 26, 27, 29, 32, 35, 40, 48, 26, 27, 29, 32, 35, 40, 48, 58, 26, 27, 29, 34, 38, 46, 56, 69, 27, 29, 35, 38, 46, 56, 69, 83 };

typedef struct { const uint8_t *p; size_t n, bit; } Bits;
static unsigned rb(Bits *b, int k)
{
    unsigned v = 0;
    while (k--) { v <<= 1; if (b->bit < b->n * 8) v |= (b->p[b->bit >> 3] >> (7 - (b->bit & 7))) & 1; b->bit++; }
    return v;
}
static void read_matrix(Bits *b, uint8_t *m) { for (int i = 0; i < 64; i++) m[zigzag[i]] = (uint8_t)rb(b, 8); }

bool mpeg2_next_unit(const uint8_t *buf, size_t n, size_t *pos, int *code, const uint8_t **data, size_t *len)
{
    size_t i = *pos;
    while (i + 3 < n && !(buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1)) i++;
    if (i + 3 >= n) return false;
    *code = buf[i + 3]; const size_t s = i + 4;
    size_t e = s;
    while (e + 2 < n && !(buf[e] == 0 && buf[e + 1] == 0 && buf[e + 2] == 1)) e++;
    if (e + 2 >= n) e = n;
    *data = buf + s; *len = e - s; *pos = e;
    return true;
}

int mpeg2dec_open(Mpeg2Dec *d, Rm *rm, unsigned maxW, unsigned maxH)
{
    memset(d, 0, sizeof *d);
    d->rm = rm; d->maxW = (maxW + 15) & ~15u; d->maxH = (maxH + 31) & ~31u; d->fwd = d->bwd = -1; d->fieldSurf = -1;
    d->pitch = ((size_t)d->maxW + 63) & ~(size_t)63;
    NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = 0 };
    if (chan_create(rm, &d->chan, NV2080_ENGINE_TYPE_NVDEC0, rm_class(rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "mpeg2dec: channel failed\n"); return -1; }
    d->auCap = (size_t)d->maxW * d->maxH * 2 > (4u << 20) ? (size_t)d->maxW * d->maxH * 2 : (4u << 20);
    if (!(d->au = malloc(d->auCap))) return -1;
    const size_t luma = nvenc_in_plane_size(d->pitch, d->maxH) + 65536, chroma = nvenc_in_plane_size(d->pitch, d->maxH / 2) + 65536;
    struct { RmMem *m; size_t size; } b[] = {
        { &d->setup, 0x10000 }, { &d->bs, d->auCap + 0x10000 }, { &d->offs, 4 * 4096 + 0x1000 }, { &d->status, 0x1000 },
        { &d->hist, 8u << 20 }, { &d->sem, 0x1000 } };
    for (size_t i = 0; i < sizeof b / sizeof b[0]; i++)
        if (rm_mem_alloc(rm, b[i].m, (b[i].size + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "mpeg2dec: alloc failed\n"); return -1; }
    for (int i = 0; i < MPEG2_SURFACES; i++)
        if (rm_mem_alloc(rm, &d->luma[i], (luma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &d->chroma[i], (chroma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "mpeg2dec: surface alloc failed\n"); return -1; }
    memset(d->hist.cpu, 0, d->hist.size);
    chan_mthd(&d->chan, 0, NVC56F_SET_OBJECT, 1, rm_class(rm, RM_CLS_NVDEC));
    return 0;
}

void mpeg2dec_close(Mpeg2Dec *d)
{
    chan_destroy(&d->chan);
    RmMem *m[] = { &d->setup, &d->bs, &d->offs, &d->status, &d->hist, &d->sem };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) rm_mem_free(d->rm, m[i]);
    for (int i = 0; i < MPEG2_SURFACES; i++) { rm_mem_free(d->rm, &d->luma[i]); rm_mem_free(d->rm, &d->chroma[i]); }
    free(d->au); d->au = NULL;
}

void mpeg2dec_release(Mpeg2Dec *d, int surf) { if (surf >= 0 && surf < MPEG2_SURFACES) d->busy[surf] = false; }

void mpeg2dec_read_nv12(Mpeg2Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h)
{
    nvenc_in_untile_plane(y, ystride, d->luma[surf].cpu, w, h, d->pitch);
    nvenc_in_untile_plane(uv, uvstride, d->chroma[surf].cpu, w, (h + 1) / 2, d->pitch);
}

void mpeg2dec_read_i420(Mpeg2Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h)
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

static int free_surface(Mpeg2Dec *d)
{
    for (int i = 0; i < MPEG2_SURFACES; i++) if (!d->busy[i] && i != d->fwd && i != d->bwd) return i;
    return -1;
}

// the collected picture (all its slices) through the engine
static int decode_pending(Mpeg2Dec *d)
{
    if (!d->inPic) return 0;
    d->inPic = false;
    if (!d->auSlices) return 0;
    const Mpeg2Seq *s = &d->seq; const Mpeg2Pic *p = &d->pic;
    if ((unsigned)s->width > d->maxW || (unsigned)s->height > d->maxH) { fprintf(stderr, "mpeg2dec: %dx%d is larger than the session\n", s->width, s->height); return -1; }
    if (s->chroma_format != 1) { fprintf(stderr, "mpeg2dec: only 4:2:0 is supported\n"); return -1; }
    const bool field = p->picture_structure != 3, second = field && d->fieldSurf >= 0;
    const bool anchor = p->coding_type == 1 || p->coding_type == 2;
    int surf = second ? d->fieldSurf : free_surface(d);
    if (surf < 0) { fprintf(stderr, "mpeg2dec: no free surface (the caller holds too many)\n"); return -1; }
    // references (7.6.3.5 / 7.6.3.6): P: the newest anchor; B: older (forward) and newer (backward) anchors. For the first
    // field of an anchor frame the anchors are not updated yet; the second field of a P frame may also use the first.
    int fref = -1, bref = -1;
    if (p->coding_type == 2) fref = d->bwd;
    else if (p->coding_type == 3) { fref = d->fwd; bref = d->bwd; }
    if (p->coding_type == 2 && second) fref = d->bwd;   // the engine picks the same-frame field itself (secondfield)

    static const uint8_t eos[16] = { 0, 0, 1, 0xb7, 0, 0, 1, 0xb7 };
    memcpy(d->bs.cpu, d->au, d->auN);
    memcpy(d->offs.cpu, d->auOff, sizeof(NvU32) * (size_t)d->auSlices);
    nvdec_mpeg2_pic_s *ps = d->setup.cpu; memset(ps, 0, sizeof *ps);
    memcpy(ps->eos, eos, 16); ps->explicitEOSPresentFlag = getenv("MPEG2_NO_EOS") ? 0 : 1;
    ps->stream_len = (unsigned)d->auN; ps->slice_count = (unsigned)d->auSlices;
    // interlaced sequences code the height rounded up to 32 lines (6.3.3): the engine counts macroblock rows from
    // FrameHeight, so 720 lines with progressive_sequence 0 need 736 (46 rows), else every picture ends in a timeout
    // with status 0x11 (T14.01)
    ps->FrameWidth = (short)s->width; ps->FrameHeight = (short)(s->progressive_sequence ? s->height : (s->height + 31) & ~31);
    ps->picture_structure = (unsigned char)p->picture_structure; ps->picture_coding_type = (unsigned char)p->coding_type;
    ps->intra_dc_precision = (unsigned char)p->intra_dc_precision;
    ps->frame_pred_frame_dct = (char)p->frame_pred_frame_dct; ps->concealment_motion_vectors = (char)p->concealment_motion_vectors;
    ps->intra_vlc_format = (char)p->intra_vlc_format;
    ps->tileFormat = 1; ps->gob_height = 0;
    for (int i = 0; i < 4; i++) ps->f_code[i] = (char)p->f_code[i >> 1][i & 1];
    const unsigned wmb = ((unsigned)s->width + 15) / 16, hmb = s->progressive_sequence ? ((unsigned)s->height + 15) / 16 : 2 * (((unsigned)s->height + 31) / 32);
    ps->PicWidthInMbs = (unsigned short)wmb; ps->FrameHeightInMbs = (unsigned short)hmb;
    ps->pitch_luma = ps->pitch_chroma = (unsigned)d->pitch;
    ps->luma_frame_offset = 0; ps->chroma_frame_offset = 0;
    ps->luma_top_offset = 0; ps->chroma_top_offset = 0;
    ps->luma_bot_offset = (unsigned)(d->pitch * 16 >> 8); ps->chroma_bot_offset = (unsigned)(d->pitch * 16 >> 8);   // guess: one row down
    ps->HistBufferSize = (unsigned)(d->hist.size >> 8);
    ps->alternate_scan = (unsigned short)p->alternate_scan; ps->secondfield = second;
    ps->q_scale_type = (unsigned)p->q_scale_type; ps->top_field_first = (unsigned)p->top_field_first;
    ps->full_pel_fwd_vector = (unsigned)p->full_pel[0]; ps->full_pel_bwd_vector = (unsigned)p->full_pel[1];
    memcpy(ps->quant_mat_8x8intra, s->intra, 64); memcpy(ps->quant_mat_8x8nonintra, s->nonintra, 64);   // raster order

    // picture slots (found by experiment, T14.01): 0 = the picture decoded (the engine writes slot 0 whatever
    // SET_PICTURE_INDEX says), 1 = forward reference, 2 = backward reference; an absent reference = the current surface
    const int fr = fref >= 0 ? fref : surf, br = bref >= 0 ? bref : fr;
    const int slot[3] = { surf, fr, br };
    Chan *c = &d->chan;
    chan_rewind(c);
    chan_mthd(c, 0, NVC9B0_SET_APPLICATION_ID, 1, (NvU32)NVC9B0_SET_APPLICATION_ID_ID_MPEG12);
    chan_mthd(c, 0, NVC9B0_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _CODEC_TYPE, s->mpeg1 ? NVDEC_CODEC_MPEG1 : NVDEC_CODEC_MPEG2) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _GPTIMER_ON, 1));
    chan_mthd(c, 0, NVC9B0_SET_DRV_PIC_SETUP_OFFSET, 1, hi(d->setup.gpu));
    chan_mthd(c, 0, NVC9B0_SET_IN_BUF_BASE_OFFSET, 1, hi(d->bs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_PICTURE_INDEX, 1, 0u);
    chan_mthd(c, 0, NVC9B0_SET_SLICE_OFFSETS_BUF_OFFSET, 1, hi(d->offs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_HISTORY_OFFSET, 1, hi(d->hist.gpu));
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
    if (sv[0] != val) { fprintf(stderr, "mpeg2dec: no completion (error notifier 0x%x)\n", chan_error(c)); return -1; }
    const nvdec_status_s *st = d->status.cpu;
    d->mbs_ok = st->mbs_correctly_decoded; d->mbs_err = st->mbs_in_error; d->err_status = st->error_status;
    if (d->verbose)
        printf("  dec: surf %d %c%s tref %3d fwd %d bwd %d slices %d%s%s%s -> %u MBs ok, %u err, status 0x%x, %.2f ms\n", surf, " IPBD"[p->coding_type & 7],
               field ? (second ? " field2" : " field1") : "", p->temporal_reference, fref, bref, d->auSlices, p->alternate_scan ? " altscan" : "",
               p->intra_vlc_format ? " intravlc" : "", p->q_scale_type ? " nonlinq" : "", d->mbs_ok, d->mbs_err, d->err_status, d->engine_ms);
    d->auN = 0; d->auSlices = 0;
    d->cur_surf = surf; d->cur_type = p->coding_type; d->cur_tref = p->temporal_reference; d->cur_w = s->width; d->cur_h = s->height;
    d->cur_frame_done = !field || second;
    if (field && !second) { d->fieldSurf = surf; d->busy[surf] = true; return 0; }   // wait for the second field
    d->fieldSurf = -1;
    d->busy[surf] = true;
    if (anchor) { d->fwd = d->bwd; d->bwd = surf; }
    return 1;
}

int mpeg2dec_flush(Mpeg2Dec *d) { return decode_pending(d); }

void mpeg2dec_default_sequence(Mpeg2Dec *d, int width, int height, bool mpeg1)
{
    if (d->haveSeq) return;
    Mpeg2Seq *s = &d->seq;
    s->width = width; s->height = height; s->mpeg1 = mpeg1; s->progressive_sequence = 1; s->chroma_format = 1;
    memcpy(s->intra, default_intra, 64); memset(s->nonintra, 16, 64);
    d->haveSeq = true;
}

int mpeg2dec_unit(Mpeg2Dec *d, int code, const uint8_t *data, size_t len)
{
    if (code >= 0x01 && code <= 0xaf) {                         // slice
        if (!d->inPic) return 0;
        if (d->auSlices >= 4096 || d->auN + len + 4 > d->auCap) { fprintf(stderr, "mpeg2dec: picture too large\n"); return -1; }
        d->auOff[d->auSlices++] = (NvU32)d->auN;
        d->au[d->auN++] = 0; d->au[d->auN++] = 0; d->au[d->auN++] = 1; d->au[d->auN++] = (uint8_t)code;
        memcpy(d->au + d->auN, data, len); d->auN += len;
        return 0;
    }
    int r = 0;
    if (code == 0x00 || code == 0xb3 || code == 0xb8 || code == 0xb7) r = decode_pending(d);   // the picture ends
    if (r < 0) return r;
    Bits b = { data, len, 0 };
    if (code == 0xb3) {                                         // sequence header (6.2.2.1)
        Mpeg2Seq *s = &d->seq;
        s->width = (int)rb(&b, 12); s->height = (int)rb(&b, 12); rb(&b, 4); s->frame_rate_code = (int)rb(&b, 4);
        rb(&b, 18); rb(&b, 1); rb(&b, 10); rb(&b, 1);
        if (rb(&b, 1)) read_matrix(&b, s->intra); else memcpy(s->intra, default_intra, 64);
        if (rb(&b, 1)) read_matrix(&b, s->nonintra); else memset(s->nonintra, 16, 64);
        if (!d->haveSeq) { s->mpeg1 = true; s->progressive_sequence = 1; s->chroma_format = 1; }
        d->haveSeq = true;
    } else if (code == 0xb5) {                                  // extensions (6.2.2.3 ...)
        const int id = (int)rb(&b, 4);
        if (id == 1) {
            Mpeg2Seq *s = &d->seq; s->mpeg1 = false;
            rb(&b, 8); s->progressive_sequence = (int)rb(&b, 1); s->chroma_format = (int)rb(&b, 2);
            s->width = (s->width & 0xfff) | (int)rb(&b, 2) << 12; s->height = (s->height & 0xfff) | (int)rb(&b, 2) << 12;
        } else if (id == 3) {
            if (rb(&b, 1)) read_matrix(&b, d->seq.intra);
            if (rb(&b, 1)) read_matrix(&b, d->seq.nonintra);
        } else if (id == 8 && d->skipping) {                    // a skipped field pair counts once
            const int ps_ = (int)(rb(&b, 16), rb(&b, 2), rb(&b, 2));
            if (ps_ != 3) { if (d->skipField2) d->skipped--; d->skipField2 = !d->skipField2; } else d->skipField2 = false;
        } else if (id == 8 && d->inPic) {
            Mpeg2Pic *p = &d->pic;
            p->f_code[0][0] = (int)rb(&b, 4); p->f_code[0][1] = (int)rb(&b, 4); p->f_code[1][0] = (int)rb(&b, 4); p->f_code[1][1] = (int)rb(&b, 4);
            p->intra_dc_precision = (int)rb(&b, 2); p->picture_structure = (int)rb(&b, 2); p->top_field_first = (int)rb(&b, 1);
            p->frame_pred_frame_dct = (int)rb(&b, 1); p->concealment_motion_vectors = (int)rb(&b, 1); p->q_scale_type = (int)rb(&b, 1);
            p->intra_vlc_format = (int)rb(&b, 1); p->alternate_scan = (int)rb(&b, 1); p->repeat_first_field = (int)rb(&b, 1);
            rb(&b, 1); p->progressive_frame = (int)rb(&b, 1);
        }
    } else if (code == 0x00) {                                  // picture header (6.2.3)
        if (!d->haveSeq) return 0;
        Mpeg2Pic *p = &d->pic; memset(p, 0, sizeof *p);
        p->temporal_reference = (int)rb(&b, 10); p->coding_type = (int)rb(&b, 3); rb(&b, 16);
        p->f_code[0][0] = p->f_code[0][1] = p->f_code[1][0] = p->f_code[1][1] = 15;
        if (p->coding_type == 2 || p->coding_type == 3) { p->full_pel[0] = (int)rb(&b, 1); p->f_code[0][0] = p->f_code[0][1] = (int)rb(&b, 3); }
        if (p->coding_type == 3) { p->full_pel[1] = (int)rb(&b, 1); p->f_code[1][0] = p->f_code[1][1] = (int)rb(&b, 3); }
        p->picture_structure = 3; p->frame_pred_frame_dct = 1; p->progressive_frame = 1;   // MPEG-1 values; MPEG-2 overrides
        d->skipping = (p->coding_type == 3 && (d->fwd < 0 || d->bwd < 0)) ||    // a B picture without both anchors (open
                      (p->coding_type == 2 && d->bwd < 0 && d->fieldSurf < 0);  // GOP start, broken link): left out
        if (d->skipping) { d->skipped++; return r; }   // r: this header may have finished the previous picture
        d->inPic = true; d->auN = 0; d->auSlices = 0;
    }
    return r;
}
