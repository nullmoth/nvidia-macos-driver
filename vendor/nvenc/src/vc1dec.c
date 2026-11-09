// vc1dec.c: see vc1dec.h. Section numbers refer to SMPTE 421M.
#include "vc1dec.h"
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

typedef struct { const uint8_t *p; size_t n, bit; } Bits;
static unsigned rb(Bits *b, int k)
{
    unsigned v = 0;
    while (k--) { v <<= 1; if (b->bit < b->n * 8) v |= (b->p[b->bit >> 3] >> (7 - (b->bit & 7))) & 1; b->bit++; }
    return v;
}
// an EBDU without its emulation prevention bytes (00 00 03 -> 00 00), for the headers parsed here (7.1, Annex E)
static size_t unescape(const uint8_t *in, size_t n, uint8_t *out, size_t cap)
{
    size_t o = 0; int z = 0;
    for (size_t i = 0; i < n && o < cap; i++) {
        if (z >= 2 && in[i] == 3) { z = 0; continue; }
        z = in[i] ? 0 : z + 1; out[o++] = in[i];
    }
    return o;
}

int vc1dec_open(Vc1Dec *d, Rm *rm, unsigned maxW, unsigned maxH)
{
    memset(d, 0, sizeof *d);
    d->rm = rm; d->maxW = (maxW + 15) & ~15u; d->maxH = (maxH + 31) & ~31u; d->fwd = d->bwd = -1; d->fieldSurf = -1; d->fptype = -1;
    d->pitch = ((size_t)d->maxW + 63) & ~(size_t)63;
    NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = 0 };
    if (chan_create(rm, &d->chan, NV2080_ENGINE_TYPE_NVDEC0, rm_class(rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "vc1dec: channel failed\n"); return -1; }
    d->auCap = (size_t)d->maxW * d->maxH * 2 > (4u << 20) ? (size_t)d->maxW * d->maxH * 2 : (4u << 20);
    if (!(d->au = malloc(d->auCap))) return -1;
    const size_t wmb = d->maxW / 16, hmb = d->maxH / 16;
    const size_t luma = nvenc_in_plane_size(d->pitch, d->maxH) + 65536, chroma = nvenc_in_plane_size(d->pitch, d->maxH / 2) + 65536;
    // buffer sizes after nvtegra: co-located 3 x (macroblocks with an even row count x 64), history 0x300 per (even) macroblock column
    const size_t coloc = 3 * ((wmb * ((hmb + 1) & ~(size_t)1) * 64 + 255) & ~(size_t)255), hist = ((wmb + 1) & ~(size_t)1) * 0x300;
    struct { RmMem *m; size_t size; } b[] = {
        { &d->setup, 0x10000 }, { &d->bs, d->auCap + 0x10000 }, { &d->offs, 4 * 1024 + 0x1000 }, { &d->status, 0x1000 },
        { &d->coloc, coloc }, { &d->hist, hist }, { &d->scratch, 0x1000 }, { &d->sem, 0x1000 } };
    for (size_t i = 0; i < sizeof b / sizeof b[0]; i++)
        if (rm_mem_alloc(rm, b[i].m, (b[i].size + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "vc1dec: alloc failed\n"); return -1; }
    for (int i = 0; i < VC1_SURFACES; i++)
        if (rm_mem_alloc(rm, &d->luma[i], (luma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &d->chroma[i], (chroma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "vc1dec: surface alloc failed\n"); return -1; }
    memset(d->coloc.cpu, 0, d->coloc.size); memset(d->hist.cpu, 0, d->hist.size); memset(d->scratch.cpu, 0, d->scratch.size);
    chan_mthd(&d->chan, 0, NVC56F_SET_OBJECT, 1, rm_class(rm, RM_CLS_NVDEC));
    return 0;
}

void vc1dec_close(Vc1Dec *d)
{
    chan_destroy(&d->chan);
    RmMem *m[] = { &d->setup, &d->bs, &d->offs, &d->status, &d->coloc, &d->hist, &d->scratch, &d->sem };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) rm_mem_free(d->rm, m[i]);
    for (int i = 0; i < VC1_SURFACES; i++) { rm_mem_free(d->rm, &d->luma[i]); rm_mem_free(d->rm, &d->chroma[i]); }
    free(d->au); d->au = NULL;
}

void vc1dec_release(Vc1Dec *d, int surf) { if (surf >= 0 && surf < VC1_SURFACES) d->busy[surf] = false; }

void vc1dec_read_nv12(Vc1Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h)
{
    nvenc_in_untile_plane(y, ystride, d->luma[surf].cpu, w, h, d->pitch);
    nvenc_in_untile_plane(uv, uvstride, d->chroma[surf].cpu, w, (h + 1) / 2, d->pitch);
}

void vc1dec_read_i420(Vc1Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h)
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

// Simple / Main sequence header (Annex J STRUCT_C, as in ffmpeg's and libavcodec's reading of it)
int vc1dec_simple_main_header(Vc1Dec *d, const uint8_t c[4], int width, int height)
{
    Vc1Seq *s = &d->seq; memset(s, 0, sizeof *s);
    Bits b = { c, 4, 0 };
    const int profile = (int)rb(&b, 2);
    if (profile > 1) { fprintf(stderr, "vc1dec: STRUCT_C profile %d is not Simple / Main\n", profile); return -1; }
    rb(&b, 1); const int sprite = (int)rb(&b, 1);                // res_y411, res_sprite
    rb(&b, 3); rb(&b, 5);                                         // frmrtq / bitrtq_postproc
    s->loopfilter = (int)rb(&b, 1); rb(&b, 1); s->multires = (int)rb(&b, 1); rb(&b, 1);
    s->fastuvmc = (int)rb(&b, 1); s->extended_mv = (int)rb(&b, 1); s->dquant = (int)rb(&b, 2);
    s->vstransform = (int)rb(&b, 1); rb(&b, 1); s->overlap = (int)rb(&b, 1); s->syncmarker = (int)rb(&b, 1);
    s->rangered = (int)rb(&b, 1); s->maxbframes = (int)rb(&b, 3); s->quantizer = (int)rb(&b, 2); s->finterpflag = (int)rb(&b, 1);
    if (sprite) { fprintf(stderr, "vc1dec: WMV3 sprite streams are not supported\n"); return -1; }
    s->width = width; s->height = height; s->valid = true; s->advanced = false;
    return 0;
}

static int free_surface(Vc1Dec *d)
{
    for (int i = 0; i < VC1_SURFACES; i++) if (!d->busy[i] && i != d->fwd && i != d->bwd) return i;
    return -1;
}

// one picture (frame or field) through the engine; data = the picture from its start code, with the slices
static int decode(Vc1Dec *d, const uint8_t *data, size_t n, const NvU32 *offs, int nslices, int type, bool field, bool second)
{
    const Vc1Seq *s = &d->seq;
    static const uint8_t endseq[16] = { 0, 0, 1, 0x0a, 0, 0, 0, 0, 0, 0, 1, 0x0a, 0, 0, 0, 0 };   // nvtegra's end sequence
    if ((unsigned)s->width > d->maxW || (unsigned)s->height > d->maxH) { fprintf(stderr, "vc1dec: %dx%d is larger than the session\n", s->width, s->height); return -1; }
    const bool anchor = type == 0 || type == 1;
    if (type == 2 && (d->fwd < 0 || d->bwd < 0)) return 0;
    if (type == 1 && d->bwd < 0 && !second) return 0;
    const int surf = second ? d->fieldSurf : free_surface(d);
    if (surf < 0) { fprintf(stderr, "vc1dec: no free surface\n"); return -1; }
    int fref = -1, bref = -1;
    if (type == 1) fref = d->bwd; else if (type == 2) { fref = d->fwd; bref = d->bwd; }
    const int slot[3] = { surf, fref >= 0 ? fref : surf, bref >= 0 ? bref : (fref >= 0 ? fref : surf) };

    memcpy(d->bs.cpu, data, n); memcpy((uint8_t *)d->bs.cpu + n, endseq, sizeof endseq);
    memcpy(d->offs.cpu, offs, sizeof(NvU32) * (size_t)nslices);
    nvdec_vc1_pic_s *ps = d->setup.cpu; memset(ps, 0, sizeof *ps);
    ps->stream_len = (unsigned)(n + sizeof endseq); ps->slice_count = (unsigned)nslices;
    ps->scratch_pic_buffer_size = (unsigned)d->scratch.size;
    const int align = s->advanced ? 1 : 16;
    ps->FrameWidth = ps->CodedWidth = (unsigned short)((s->width + align - 1) / align * align);
    ps->FrameHeight = ps->CodedHeight = (unsigned short)((s->height + align - 1) / align * align);
    ps->profile = s->advanced ? 2 : 1;
    ps->postprocflag = (unsigned char)s->postprocflag; ps->pulldown = (unsigned char)s->pulldown; ps->interlace = (unsigned char)s->interlace;
    ps->tfcntrflag = (unsigned char)s->tfcntrflag; ps->finterpflag = (unsigned char)s->finterpflag; ps->psf = (unsigned char)s->psf;
    ps->tileFormat = 1; ps->gob_height = 0;
    ps->multires = (unsigned char)s->multires; ps->syncmarker = (unsigned char)s->syncmarker; ps->rangered = (unsigned char)s->rangered;
    ps->maxbframes = (unsigned char)s->maxbframes;
    ps->dquant = (unsigned char)s->dquant; ps->panscan_flag = (unsigned char)s->panscan; ps->refdist_flag = (unsigned char)s->refdist_flag;
    ps->quantizer = (unsigned char)s->quantizer; ps->extended_mv = (unsigned char)s->extended_mv; ps->extended_dmv = (unsigned char)s->extended_dmv;
    ps->overlap = (unsigned char)s->overlap; ps->vstransform = (unsigned char)s->vstransform;
    ps->HistBufferSize = (unsigned)(d->hist.size >> 8);
    ps->FrameStride[0] = ps->FrameStride[1] = (unsigned)d->pitch;
    ps->loopfilter = (unsigned char)s->loopfilter; ps->fastuvmc = (unsigned char)s->fastuvmc;

    Chan *c = &d->chan;
    chan_rewind(c);
    chan_mthd(c, 0, NVC9B0_SET_APPLICATION_ID, 1, (NvU32)NVC9B0_SET_APPLICATION_ID_ID_VC1);
    chan_mthd(c, 0, NVC9B0_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _CODEC_TYPE, NVDEC_CODEC_VC1) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _ERR_CONCEAL_ON, 1) | DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _GPTIMER_ON, 1));
    chan_mthd(c, 0, NVC9B0_SET_PICTURE_INDEX, 1, (NvU32)surf);
    chan_mthd(c, 0, NVC9B0_SET_DRV_PIC_SETUP_OFFSET, 1, hi(d->setup.gpu));
    chan_mthd(c, 0, NVC9B0_SET_IN_BUF_BASE_OFFSET, 1, hi(d->bs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_SLICE_OFFSETS_BUF_OFFSET, 1, hi(d->offs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_NVDEC_STATUS_OFFSET, 1, hi(d->status.gpu));
    chan_mthd(c, 0, NVC9B0_SET_COLOC_DATA_OFFSET, 1, hi(d->coloc.gpu));
    chan_mthd(c, 0, NVC9B0_SET_HISTORY_OFFSET, 1, hi(d->hist.gpu));
    chan_mthd(c, 0, NVC9B0_SET_PIC_SCRATCH_BUF_OFFSET, 1, hi(d->scratch.gpu));
    for (int i = 0; i < 3; i++) {                               // the picture decoded, forward, backward (as MPEG-2)
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
        fprintf(stderr, "vc1dec: no completion (error notifier 0x%x)\n", chan_error(c));
        if (chan_error(c) && !chan_fault(c, &fa, &ft, fs)) fprintf(stderr, "vc1dec: MMU fault at 0x%llx type %u %s\n", (unsigned long long)fa, ft, fs);
        return -1;
    }
    const nvdec_status_s *st = d->status.cpu;
    d->mbs_ok = st->mbs_correctly_decoded; d->mbs_err = st->mbs_in_error; d->err_status = st->error_status;
    if (d->verbose)
        printf("  dec: surf %d %s%s fwd %d bwd %d slices %d %zu bytes -> %u MBs ok, %u err, status 0x%x, %.2f ms\n", surf, (const char *[]){ "I", "P", "B", "BI", "skip" }[type],
               field ? (second ? " field2" : " field1") : "", fref, bref, nslices, n, d->mbs_ok, d->mbs_err, d->err_status, d->engine_ms);
    d->cur_w = s->width; d->cur_h = s->height; d->cur_type = type; d->cur_surf = surf; d->cur_shown = true;
    if (field && !second) { d->fieldSurf = surf; d->busy[surf] = true; return 0; }
    d->fieldSurf = -1; d->busy[surf] = true;
    if (anchor) { d->fwd = d->bwd; d->bwd = surf; }
    return 1;
}

static int skipped(Vc1Dec *d)   // a skipped picture: the last anchor is shown again (7.1.1.4, PTYPE "skipped")
{
    if (d->bwd < 0) return 0;
    d->cur_type = 4; d->cur_surf = d->bwd; d->cur_shown = false; d->busy[d->bwd] = true;
    d->cur_w = d->seq.width; d->cur_h = d->seq.height;
    return 1;
}

int vc1dec_simple_main_frame(Vc1Dec *d, const uint8_t *buf, size_t len)
{
    if (!d->seq.valid) return -1;
    if (len <= 1) return skipped(d);                            // a skipped frame (no data)
    const Vc1Seq *s = &d->seq;
    Bits b = { buf, len, 0 };                                   // frame header (7.1.1.1-7.1.1.4) up to PTYPE / BFRACTION
    if (s->finterpflag) rb(&b, 1);
    rb(&b, 2);
    if (s->rangered) rb(&b, 1);
    int type;
    if (rb(&b, 1)) type = 1;
    else if (s->maxbframes && !rb(&b, 1)) {
        type = 2;
        if (rb(&b, 3) == 7 && rb(&b, 4) == 15) type = 3;      // BFRACTION 1111111 = BI picture
    } else type = 0;
    // the engine wants a frame start code before a Simple / Main frame (nvtegra)
    static uint8_t tmp[1 << 22];
    if (len + 4 > sizeof tmp) { fprintf(stderr, "vc1dec: frame too large\n"); return -1; }
    tmp[0] = 0; tmp[1] = 0; tmp[2] = 1; tmp[3] = 0x0d; memcpy(tmp + 4, buf, len);
    const NvU32 off0 = 0;
    return decode(d, tmp, len + 4, &off0, 1, type, false, false);
}

// ---- Advanced profile ----
static int parse_sequence(Vc1Dec *d, Bits *b)   // 6.1
{
    Vc1Seq *s = &d->seq; memset(s, 0, sizeof *s);
    if (rb(b, 2) != 3) { fprintf(stderr, "vc1dec: sequence header is not Advanced profile\n"); return -1; }
    rb(b, 3);                                                   // level
    if (rb(b, 2) != 1) { fprintf(stderr, "vc1dec: only 4:2:0 is supported\n"); return -1; }
    rb(b, 3); rb(b, 5);                                         // frmrtq / bitrtq_postproc
    s->postprocflag = (int)rb(b, 1);
    s->width = ((int)rb(b, 12) + 1) * 2; s->height = ((int)rb(b, 12) + 1) * 2;
    s->pulldown = (int)rb(b, 1); s->interlace = (int)rb(b, 1); s->tfcntrflag = (int)rb(b, 1); s->finterpflag = (int)rb(b, 1);
    rb(b, 1); s->psf = (int)rb(b, 1);
    if (rb(b, 1)) {                                             // display_ext
        rb(b, 14); rb(b, 14);
        if (rb(b, 1)) { if (rb(b, 4) == 15) rb(b, 16); }
        if (rb(b, 1)) { if (rb(b, 1)) rb(b, 16); else { rb(b, 8); rb(b, 4); } }
        if (rb(b, 1)) rb(b, 24);
    }
    if (rb(b, 1)) { s->hrd_buckets = (int)rb(b, 5); rb(b, 8); for (int i = 0; i < s->hrd_buckets; i++) rb(b, 32); }
    s->advanced = true; s->valid = true;
    return 0;
}

static int parse_entrypoint(Vc1Dec *d, Bits *b)   // 6.2
{
    Vc1Seq *s = &d->seq;
    rb(b, 1); rb(b, 1);                                         // broken_link, closed_entry
    s->panscan = (int)rb(b, 1); s->refdist_flag = (int)rb(b, 1); s->loopfilter = (int)rb(b, 1);
    s->fastuvmc = (int)rb(b, 1); s->extended_mv = (int)rb(b, 1); s->dquant = (int)rb(b, 2);
    s->vstransform = (int)rb(b, 1); s->overlap = (int)rb(b, 1); s->quantizer = (int)rb(b, 2);
    for (int i = 0; i < s->hrd_buckets; i++) rb(b, 8);
    if (rb(b, 1)) { s->width = ((int)rb(b, 12) + 1) * 2; s->height = ((int)rb(b, 12) + 1) * 2; }
    if (s->extended_mv) s->extended_dmv = (int)rb(b, 1);
    if ((s->range_mapy_flag = (int)rb(b, 1))) rb(b, 3);
    if ((s->range_mapuv_flag = (int)rb(b, 1))) rb(b, 3);
    if ((s->range_mapy_flag || s->range_mapuv_flag) && d->verbose) printf("  vc1dec: range mapping is not applied (no display path)\n");
    if (d->verbose > 1) printf("  entry: %dx%d interlace %d pulldown %d tfcntr %d finterp %d psf %d postproc %d hrd %d | panscan %d refdist %d loopfilter %d fastuvmc %d extmv %d dquant %d vstransform %d overlap %d quantizer %d extdmv %d\n",
        s->width, s->height, s->interlace, s->pulldown, s->tfcntrflag, s->finterpflag, s->psf, s->postprocflag, s->hrd_buckets, s->panscan, s->refdist_flag,
        s->loopfilter, s->fastuvmc, s->extended_mv, s->dquant, s->vstransform, s->overlap, s->quantizer, s->extended_dmv);
    return 0;
}

// the type of the collected picture from its header (7.1.1.1-7.1.1.4 / 9.1.1): 0 I, 1 P, 2 B, 3 BI, 4 skipped; *field
static int picture_type(Vc1Dec *d, const uint8_t *hdr, size_t n, bool *field, bool second)
{
    uint8_t buf[64]; const size_t m = unescape(hdr, n < 64 ? n : 64, buf, sizeof buf);
    Bits b = { buf, m, 0 };
    int fcm = 0;
    if (d->seq.interlace) { if (rb(&b, 1)) fcm = rb(&b, 1) ? 3 : 2; }
    *field = fcm == 3;
    if (fcm == 3) {                                             // FPTYPE: the types of both fields (Table 105)
        static const int first[8] = { 0, 0, 1, 1, 2, 2, 3, 3 }, sec[8] = { 0, 1, 0, 1, 2, 3, 2, 3 };
        const int fp = (int)rb(&b, 3);
        return second ? sec[fp] : first[fp];
    }
    if (!rb(&b, 1)) return 1;
    if (!rb(&b, 1)) return 2;
    if (!rb(&b, 1)) return 0;
    if (!rb(&b, 1)) return 3;
    return 4;
}

static int decode_pending(Vc1Dec *d)
{
    if (!d->inPic) return 0;
    d->inPic = false;
    if (!d->auN) return 0;
    const bool second = d->au[3] == 0x0c;
    bool field = false; int type;
    if (second) {                                               // the field picture header follows the 0x0C start code
        static const int sec[8] = { 0, 1, 0, 1, 2, 3, 2, 3 };
        type = d->fptype >= 0 ? sec[d->fptype] : 1; field = true;
        if (d->fieldSurf < 0) return 0;
    } else {
        type = picture_type(d, d->au + 4, d->auN - 4, &field, false);
        if (field) { uint8_t buf[16]; const size_t m = unescape(d->au + 4, d->auN - 4 < 16 ? d->auN - 4 : 16, buf, 16); Bits b = { buf, m, 2 }; d->fptype = (int)rb(&b, 3); }
    }
    if (type == 4) { d->auN = 0; return skipped(d); }
    const int r = decode(d, d->au, d->auN, d->auOff, d->auSlices, type, field, second);
    d->auN = 0; d->auSlices = 0;
    return r;
}

int vc1dec_flush(Vc1Dec *d) { return decode_pending(d); }

int vc1dec_advanced_unit(Vc1Dec *d, const uint8_t *start, size_t len)
{
    if (len < 4) return 0;
    const int code = start[3];
    if (code == 0x0b && d->inPic) {                             // slice: part of the picture being collected
        if (d->auSlices >= 1024 || d->auN + len > d->auCap) { fprintf(stderr, "vc1dec: picture too large\n"); return -1; }
        d->auOff[d->auSlices++] = (NvU32)d->auN;
        memcpy(d->au + d->auN, start, len); d->auN += len;
        return 0;
    }
    if (code >= 0x1b && code <= 0x1f) return 0;                 // user data
    const int r = decode_pending(d);
    if (r < 0) return r;
    uint8_t buf[256]; const size_t m = unescape(start + 4, len - 4 < sizeof buf ? len - 4 : sizeof buf, buf, sizeof buf);
    Bits b = { buf, m, 0 };
    if (code == 0x0f) { if (parse_sequence(d, &b)) return -1; }
    else if (code == 0x0e) { if (d->seq.valid) parse_entrypoint(d, &b); }
    else if ((code == 0x0d || code == 0x0c) && d->seq.valid) {
        if (len > d->auCap) { fprintf(stderr, "vc1dec: picture too large\n"); return -1; }
        memcpy(d->au, start, len); d->auN = len; d->auOff[0] = 0; d->auSlices = 1; d->inPic = true;
    }
    return r;
}
