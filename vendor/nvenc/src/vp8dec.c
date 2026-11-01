// vp8dec.c: see vp8dec.h. Section numbers refer to RFC 6386.
#include "vp8dec.h"
#include "vp8tables.h"
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

// ---- boolean decoder (7.3) ----
typedef struct { const uint8_t *b, *e; unsigned value, range; int bits; } Bd;
static void bd_init(Bd *d, const uint8_t *b, size_t n)
{
    d->b = b; d->e = b + n; d->value = 0; d->range = 255; d->bits = 0;
    for (int i = 0; i < 2; i++) d->value = (d->value << 8) | (d->b < d->e ? *d->b++ : 0);
}
static int bd(Bd *d, int prob)
{
    const unsigned split = 1 + (((d->range - 1) * (unsigned)prob) >> 8), SPLIT = split << 8;
    int bit;
    if (d->value >= SPLIT) { bit = 1; d->range -= split; d->value -= SPLIT; } else { bit = 0; d->range = split; }
    while (d->range < 128) {
        d->value <<= 1; d->range <<= 1;
        if (++d->bits == 8) { d->bits = 0; d->value |= d->b < d->e ? *d->b++ : 0; }
    }
    return bit;
}
static unsigned L(Bd *d, int n) { unsigned v = 0; while (n--) v = (v << 1) | (unsigned)bd(d, 128); return v; }
static void skip_signed(Bd *d, int n) { if (L(d, 1)) { L(d, n); L(d, 1); } }   // flag, magnitude, sign

int vp8_parse_header(const uint8_t *buf, size_t len, Vp8Header *h)
{
    memset(h, 0, sizeof *h);
    if (len < 3) return -1;
    const unsigned tag = buf[0] | buf[1] << 8 | buf[2] << 16;
    h->key = !(tag & 1); h->version = (int)(tag >> 1) & 7; h->show = (int)(tag >> 4) & 1; h->first_part_size = (int)(tag >> 5);
    size_t off = 3;
    if (h->key) {
        if (len < 10 || buf[3] != 0x9d || buf[4] != 0x01 || buf[5] != 0x2a) return -1;
        h->width = (buf[6] | buf[7] << 8) & 0x3fff; h->height = (buf[8] | buf[9] << 8) & 0x3fff;
        off = 10;
    }
    if (off + (size_t)h->first_part_size > len) return -1;
    Bd b; bd_init(&b, buf + off, (size_t)h->first_part_size);
    if (h->key) { L(&b, 1); L(&b, 1); }                         // color_space, clamping_type
    h->segmentation = (int)L(&b, 1);
    if (h->segmentation) {                                      // 9.3
        const int map = (int)L(&b, 1), data = (int)L(&b, 1);
        h->update_seg_data = data;
        if (data) { L(&b, 1); for (int i = 0; i < 4; i++) skip_signed(&b, 7); for (int i = 0; i < 4; i++) skip_signed(&b, 6); }
        if (map) for (int i = 0; i < 3; i++) if (L(&b, 1)) L(&b, 8);
    }
    L(&b, 1); h->filter_level = (int)L(&b, 6); L(&b, 3);        // 9.6: filter_type, loop_filter_level, sharpness_level
    if (L(&b, 1) && L(&b, 1)) for (int i = 0; i < 8; i++) skip_signed(&b, 6);   // loop filter deltas
    h->partitions = 1 << L(&b, 2);                              // 9.5
    L(&b, 7); for (int i = 0; i < 5; i++) skip_signed(&b, 4);   // 9.6 quantizer indices
    if (h->key) { h->refresh_golden = h->refresh_alt = h->refresh_last = 1; h->refresh_entropy = (int)L(&b, 1); }
    else {                                                      // 9.7, 9.8
        h->refresh_golden = (int)L(&b, 1); h->refresh_alt = (int)L(&b, 1);
        if (!h->refresh_golden) h->copy_to_golden = (int)L(&b, 2);
        if (!h->refresh_alt) h->copy_to_alt = (int)L(&b, 2);
        h->sign_bias_golden = (int)L(&b, 1); h->sign_bias_alt = (int)L(&b, 1);
        h->refresh_entropy = (int)L(&b, 1); h->refresh_last = (int)L(&b, 1);
    }
    return 0;
}

int vp8dec_open(Vp8Dec *d, Rm *rm, unsigned maxW, unsigned maxH)
{
    memset(d, 0, sizeof *d);
    d->rm = rm; d->maxW = (maxW + 15) & ~15u; d->maxH = (maxH + 15) & ~15u; d->golden = d->altref = d->last = -1;
    d->pitch = ((size_t)d->maxW + 63) & ~(size_t)63;
    NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = 0 };
    if (chan_create(rm, &d->chan, NV2080_ENGINE_TYPE_NVDEC0, rm_class(rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "vp8dec: channel failed\n"); return -1; }
    const size_t bs = (size_t)d->maxW * d->maxH * 2 > (4u << 20) ? (size_t)d->maxW * d->maxH * 2 : (4u << 20);
    const size_t luma = nvenc_in_plane_size(d->pitch, d->maxH) + 65536, chroma = nvenc_in_plane_size(d->pitch, d->maxH / 2) + 65536;
    struct { RmMem *m; size_t size; } b[] = {
        { &d->setup, 0x10000 }, { &d->bs, bs + 0x10000 }, { &d->prob, 0x5000 },
        { &d->hist, (size_t)(d->maxW / 16) * 0x200 + 0x1000 }, { &d->status, 0x1000 }, { &d->sem, 0x1000 } };
    for (size_t i = 0; i < sizeof b / sizeof b[0]; i++)
        if (rm_mem_alloc(rm, b[i].m, (b[i].size + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "vp8dec: alloc failed\n"); return -1; }
    for (int i = 0; i < VP8_SURFACES; i++)
        if (rm_mem_alloc(rm, &d->luma[i], (luma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &d->chroma[i], (chroma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "vp8dec: surface alloc failed\n"); return -1; }
    // the probability buffer, set to the defaults once; the engine keeps it from then on (key frame resets, discarding a
    // frame's updates when refresh_entropy_probs is 0): coefficient probabilities [4][8][3] in 12-byte entries (11 + pad),
    // the inter luma mode (4) and chroma mode (3 + pad) probabilities, then the two MV contexts in 20 bytes each (19 + pad)
    memset(d->prob.cpu, 0, d->prob.size); memset(d->hist.cpu, 0, d->hist.size);
    uint8_t *p = d->prob.cpu;
    for (int i = 0; i < 4; i++) for (int j = 0; j < 8; j++) for (int k = 0; k < 3; k++) { memcpy(p, vp8_default_coeff_probs[i][j][k], 11); p += 12; }
    memcpy(p, vp8_default_ymode_probs, 4); p += 4;
    memcpy(p, vp8_default_uv_mode_probs, 3); p += 4;
    for (int i = 0; i < 2; i++) { memcpy(p, vp8_default_mv_probs[i], 19); p += 20; }
    chan_mthd(&d->chan, 0, NVC56F_SET_OBJECT, 1, rm_class(rm, RM_CLS_NVDEC));
    return 0;
}

void vp8dec_close(Vp8Dec *d)
{
    chan_destroy(&d->chan);
    RmMem *m[] = { &d->setup, &d->bs, &d->prob, &d->hist, &d->status, &d->sem };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) rm_mem_free(d->rm, m[i]);
    for (int i = 0; i < VP8_SURFACES; i++) { rm_mem_free(d->rm, &d->luma[i]); rm_mem_free(d->rm, &d->chroma[i]); }
}

void vp8dec_release(Vp8Dec *d, int surf) { if (surf >= 0 && surf < VP8_SURFACES) d->busy[surf] = false; }

void vp8dec_read_nv12(Vp8Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h)
{
    nvenc_in_untile_plane(y, ystride, d->luma[surf].cpu, w, h, d->pitch);
    nvenc_in_untile_plane(uv, uvstride, d->chroma[surf].cpu, w, (h + 1) / 2, d->pitch);
}

void vp8dec_read_i420(Vp8Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h)
{
    const uint8_t *Lp = d->luma[surf].cpu, *C = d->chroma[surf].cpu;
    const unsigned cw = (w + 1) / 2, ch = (h + 1) / 2;
    uint8_t *u = dst + (size_t)w * h, *v = u + (size_t)cw * ch;
    for (unsigned y = 0; y < h; y++) for (unsigned x = 0; x < w; x += 16) {
        unsigned n = w - x < 16 ? w - x : 16;
        memcpy(dst + (size_t)y * w + x, Lp + nvenc_in_offset(x, y, d->pitch), n);
    }
    for (unsigned y = 0; y < ch; y++) for (unsigned x = 0; x < cw; x++) {
        u[(size_t)y * cw + x] = C[nvenc_in_offset(2 * x, y, d->pitch)];
        v[(size_t)y * cw + x] = C[nvenc_in_offset(2 * x + 1, y, d->pitch)];
    }
}

static int free_surface(Vp8Dec *d)
{
    for (int i = 0; i < VP8_SURFACES; i++) if (!d->busy[i] && i != d->golden && i != d->altref && i != d->last) return i;
    return -1;
}

int vp8dec_frame(Vp8Dec *d, const uint8_t *buf, size_t len)
{
    Vp8Header *h = &d->h;
    if (vp8_parse_header(buf, len, h)) { fprintf(stderr, "vp8dec: bad frame header\n"); return -1; }
    if (h->key) { d->width = h->width; d->height = h->height; }
    else if (d->last < 0) return 0;                             // no key frame yet
    if ((unsigned)d->width > d->maxW || (unsigned)d->height > d->maxH) { fprintf(stderr, "vp8dec: %dx%d is larger than the session\n", d->width, d->height); return -1; }
    const int surf = free_surface(d);
    if (surf < 0) { fprintf(stderr, "vp8dec: no free surface\n"); return -1; }
    const int g = d->golden >= 0 ? d->golden : surf, a = d->altref >= 0 ? d->altref : surf, l = d->last >= 0 ? d->last : surf;

    const size_t off = h->key ? 10 : 3;                         // the engine reads from the first partition on
    memcpy(d->bs.cpu, buf + off, len - off);
    static uint8_t before[0x5000]; memcpy(before, d->prob.cpu, sizeof before);
    nvdec_vp8_pic_s *ps = d->setup.cpu; memset(ps, 0, sizeof *ps);
    ps->FrameWidth = (unsigned short)((d->width + 15) & ~15); ps->FrameHeight = (unsigned short)((d->height + 15) & ~15);
    ps->keyFrame = (unsigned char)h->key; ps->version = (unsigned char)h->version;
    ps->tileFormat = 1; ps->gob_height = 0; ps->errorConcealOn = 1;
    ps->firstPartSize = (unsigned)h->first_part_size;
    ps->HistBufferSize = (unsigned)(d->hist.size >> 8); ps->VLDBufferSize = (unsigned)(len - off);
    ps->FrameStride[0] = ps->FrameStride[1] = (unsigned)(d->pitch / 16);   // in units of 16 bytes
    ps->segmentation_feature_data_update = (unsigned char)(h->segmentation && h->update_seg_data);
    ps->reserved1[0] = ps->segmentation_feature_data_update;   // nvtegra sets both (the field may sit one byte later)

    Chan *c = &d->chan;
    chan_rewind(c);
    chan_mthd(c, 0, NVC9B0_SET_APPLICATION_ID, 1, (NvU32)NVC9B0_SET_APPLICATION_ID_ID_VP8);
    chan_mthd(c, 0, NVC9B0_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _CODEC_TYPE, NVDEC_CODEC_VP8) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _ERR_CONCEAL_ON, 1) | DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _GPTIMER_ON, 1));
    chan_mthd(c, 0, NVC9B0_SET_DRV_PIC_SETUP_OFFSET, 1, hi(d->setup.gpu));
    chan_mthd(c, 0, NVC9B0_SET_IN_BUF_BASE_OFFSET, 1, hi(d->bs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_PICTURE_INDEX, 1, 3u);
    chan_mthd(c, 0, NVC9B0_SET_HISTORY_OFFSET, 1, hi(d->hist.gpu));
    chan_mthd(c, 0, NVC9B0_SET_NVDEC_STATUS_OFFSET, 1, hi(d->status.gpu));
    chan_mthd(c, 0, NVC9B0_VP8_SET_PROB_DATA_OFFSET, 1, hi(d->prob.gpu));
    const int slot[4] = { g, a, l, surf };                      // VP8_FRAME_SFC_ID: golden, altref, last, current
    for (int i = 0; i < 4; i++) {
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
        fprintf(stderr, "vp8dec: no completion (error notifier 0x%x)\n", chan_error(c));
        if (chan_error(c) && !chan_fault(c, &fa, &ft, fs)) fprintf(stderr, "vp8dec: MMU fault at 0x%llx type %u %s\n", (unsigned long long)fa, ft, fs);
        return -1;
    }
    const nvdec_status_s *st = d->status.cpu;
    d->mbs_ok = st->mbs_correctly_decoded; d->mbs_err = st->mbs_in_error; d->err_status = st->error_status; d->result = ps->resultValue;
    if (d->verbose > 1) {   // what did the engine write into the probability buffer?
        const uint8_t *q = d->prob.cpu; size_t n = 0, f0 = 0, f1 = 0;
        for (size_t k = 0; k < sizeof before; k++) if (q[k] != before[k]) { if (!n) f0 = k; n++; f1 = k; }
        printf("    prob buffer: %zu bytes changed, 0x%zx..0x%zx\n", n, f0, f1);
    }
    if (d->verbose)
        printf("  dec: surf %d %s v%d %s g %d a %d l %d refresh g%d a%d l%d copy g%d a%d bias %d%d entropy %d parts %d -> %u MBs ok, %u err, status 0x%x, result 0x%x, %.2f ms\n",
               surf, h->key ? "KEY" : "inter", h->version, h->show ? "shown" : "hidden", g, a, l, h->refresh_golden, h->refresh_alt, h->refresh_last,
               h->copy_to_golden, h->copy_to_alt, h->sign_bias_golden, h->sign_bias_alt, h->refresh_entropy, h->partitions,
               d->mbs_ok, d->mbs_err, d->err_status, d->result, d->engine_ms);
    // reference updates (9.7, 9.8): copies take the references as they were before this frame (as libvpx / ffmpeg)
    const int og = d->golden, oa = d->altref, ol = d->last;
    if (!h->key) {
        if (h->copy_to_golden == 1) d->golden = ol; else if (h->copy_to_golden == 2) d->golden = oa;
        if (h->copy_to_alt == 1) d->altref = ol; else if (h->copy_to_alt == 2) d->altref = og;
    }
    if (h->refresh_golden) d->golden = surf;
    if (h->refresh_alt) d->altref = surf;
    if (h->refresh_last) d->last = surf;
    d->cur_surf = surf; d->busy[surf] = true;
    return 1;
}
