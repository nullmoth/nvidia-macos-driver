// vp9dec.c: see vp9dec.h. Section numbers refer to the VP9 specification v0.7.
#include "vp9dec.h"
#include "vp9tables.h"
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

int vp9dec_open(Vp9Dec *d, Rm *rm, unsigned maxW, unsigned maxH, int bit_depth)
{
    memset(d, 0, sizeof *d);
    d->rm = rm; d->maxW = (maxW + 63) & ~63u; d->maxH = (maxH + 63) & ~63u;
    d->bit_depth = bit_depth; d->bps = bit_depth > 8 ? 2 : 1;
    d->turing = rm_class(rm, RM_CLS_NVDEC) == 0xc4b0;
    d->pitch = (size_t)d->maxW * (size_t)d->bps;
    for (int i = 0; i < 8; i++) d->ref[i] = -1;
    vp9_init(&d->st);
    NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = 0 };
    if (chan_create(rm, &d->chan, NV2080_ENGINE_TYPE_NVDEC0, rm_class(rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "vp9dec: channel failed\n"); return -1; }
    const size_t sbs = (size_t)(d->maxW / 64) * (d->maxH / 64);
    // sizes measured (T10.02): column MVs 1024 bytes per superblock, the intra top line ~4 bytes per pixel of width
    // (15 352 at 3840; 8 per pixel allocated for 10-bit), the filter "above" area ~57 bytes per row at 4K
    // filter buffer: deblocking top + left lines (nvdec_drv.h's sizes), then the filter "above" and BSD control areas,
    // whose sizes the headers do not give (generous: 16 bytes per row per tile column, 64 tile columns; 4 per row)
    const size_t top = VP9_DBLK_TOP_BUF_SIZE(d->maxW), left = VP9_DBLK_LEFT_BUF_SIZE(d->maxH);
    d->flt_above = NVDEC_ALIGN(top + left);
    d->flt_bsd = d->flt_above + NVDEC_ALIGN((size_t)d->maxH * GIP_ASIC_VERT_FILTER_RAM_SIZE * 64);
    const size_t flt = d->flt_bsd + NVDEC_ALIGN((size_t)d->maxH * GIP_ASIC_BSD_CTRL_RAM_SIZE * 64);
    const size_t bsCap = (size_t)d->maxW * d->maxH * 2 > (4u << 20) ? (size_t)d->maxW * d->maxH * 2 : (4u << 20);
    struct { RmMem *m; size_t size; } b[] = {
        { &d->setup, 0x2000 }, { &d->bs, bsCap }, { &d->probs, sizeof(nvdec_vp9EntropyProbs_t) },
        { &d->counts, sizeof(nvdec_vp9EntropyCounts_t) }, { &d->seg[0], sbs * GIP_ASIC_VP9_CTB_SEG_SIZE },
        { &d->seg[1], sbs * GIP_ASIC_VP9_CTB_SEG_SIZE }, { &d->tiles, 0x1000 }, { &d->colmv[0], sbs * 1024 },
        { &d->colmv[1], sbs * 1024 }, { &d->filter, flt }, { &d->intra, (size_t)d->maxW * 8 * (size_t)d->bps }, { &d->status, 0x1000 }, { &d->sem, 0x1000 } };
    for (size_t i = 0; i < sizeof b / sizeof b[0]; i++) {
        if (rm_mem_alloc(rm, b[i].m, (b[i].size + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "vp9dec: alloc failed\n"); return -1; }
        memset(b[i].m->cpu, 0, b[i].m->size);
    }
    const size_t luma = nvenc_in_plane_size(d->pitch, d->maxH) + 65536, chroma = nvenc_in_plane_size(d->pitch, d->maxH / 2) + 65536;
    for (int i = 0; i < VP9_SURFACES; i++)
        if (rm_mem_alloc(rm, &d->luma[i], (luma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &d->chroma[i], (chroma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "vp9dec: surface alloc failed\n"); return -1; }
    chan_mthd(&d->chan, 0, NVC56F_SET_OBJECT, 1, rm_class(rm, RM_CLS_NVDEC));
    return 0;
}

void vp9dec_close(Vp9Dec *d)
{
    chan_destroy(&d->chan);
    RmMem *m[] = { &d->setup, &d->bs, &d->probs, &d->counts, &d->seg[0], &d->seg[1], &d->tiles, &d->colmv[0], &d->colmv[1],
                   &d->filter, &d->intra, &d->status, &d->sem };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) rm_mem_free(d->rm, m[i]);
    for (int i = 0; i < VP9_SURFACES; i++) { rm_mem_free(d->rm, &d->luma[i]); rm_mem_free(d->rm, &d->chroma[i]); }
}

void vp9dec_release(Vp9Dec *d, int surf) { if (surf >= 0 && surf < VP9_SURFACES) d->busy[surf] = false; }

static void copy_plane(uint8_t *dst, size_t dpitch, const uint8_t *src, size_t spitch, size_t bytes, size_t rows)
{
    for (size_t y = 0; y < rows; y++) for (size_t x = 0; x < bytes; x += 16) memcpy(dst + nvenc_in_offset(x, y, dpitch), src + nvenc_in_offset(x, y, spitch), 16);
}

int vp9dec_migrate(Vp9Dec *to, const Vp9Dec *from)
{
    if (to->bit_depth != from->bit_depth || to->maxW < from->maxW || to->maxH < from->maxH) return -1;
    to->st = from->st;
    to->have_last = from->have_last; to->last_w = from->last_w; to->last_h = from->last_h; to->last_show = from->last_show;
    // every surface a slot refers to, with its padding (the decoded size rounded up to superblocks)
    for (int i = 0; i < 8; i++) to->ref[i] = from->ref[i];
    for (int sfc = 0; sfc < VP9_SURFACES; sfc++) {
        bool used = false;
        for (int k = 0; k < 8; k++) used |= from->ref[k] == sfc;
        if (!used) continue;
        const size_t bytes = (size_t)((from->w[sfc] + 63) & ~63u) * (size_t)from->bps, rows = (from->h[sfc] + 63) & ~63u;
        copy_plane(to->luma[sfc].cpu, to->pitch, from->luma[sfc].cpu, from->pitch, bytes, rows);
        copy_plane(to->chroma[sfc].cpu, to->pitch, from->chroma[sfc].cpu, from->pitch, bytes, rows / 2);
        to->w[sfc] = from->w[sfc]; to->h[sfc] = from->h[sfc];
    }
    // the segmentation map and column MVs only matter for a frame of the same size, which this one will not be
    // (UsePrevFrameMvs is off and the map is cleared on a size change, 7.2.6)
    return 0;
}

void vp9dec_read_nv12(Vp9Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h)
{
    const size_t b = (size_t)d->bps;
    nvenc_in_untile_plane(y, ystride, d->luma[surf].cpu, w * b, h, d->pitch);
    nvenc_in_untile_plane(uv, uvstride, d->chroma[surf].cpu, ((w + 1) & ~1u) * b, (h + 1) / 2, d->pitch);
}

static unsigned sample(const uint8_t *p, size_t x, size_t y, size_t pitch, int bps, int shift)
{
    if (bps == 1) return p[nvenc_in_offset(x, y, pitch)];
    const size_t o = nvenc_in_offset(2 * x, y, pitch);      // a 16-bit sample never straddles a 16-byte run
    return (unsigned)(p[o] | p[o + 1] << 8) >> shift;
}

void vp9dec_read_i420(Vp9Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h)
{
    const uint8_t *L = d->luma[surf].cpu, *C = d->chroma[surf].cpu;
    const unsigned cw = (w + 1) / 2, ch = (h + 1) / 2;
    const int b = d->bps, sh = b == 2 ? 16 - d->bit_depth : 0;   // the engine writes 10/12-bit samples MSB-aligned (like P010)
    #define PUT(i, v) do { if (b == 1) dst[i] = (uint8_t)(v); else { dst[2 * (i)] = (uint8_t)(v); dst[2 * (i) + 1] = (uint8_t)((v) >> 8); } } while (0)
    const size_t U = (size_t)w * h, V = U + (size_t)cw * ch;
    for (unsigned y = 0; y < h; y++) for (unsigned x = 0; x < w; x++) PUT((size_t)y * w + x, sample(L, x, y, d->pitch, b, sh));
    for (unsigned y = 0; y < ch; y++) for (unsigned x = 0; x < cw; x++) {
        PUT(U + (size_t)y * cw + x, sample(C, 2 * x, y, d->pitch, b, sh));
        PUT(V + (size_t)y * cw + x, sample(C, 2 * x + 1, y, d->pitch, b, sh));
    }
    #undef PUT
}

// the probability table as NVDEC reads it: 9-probability tables split 8 + 1, rows padded to 4, the key frame partition
// probabilities in partition_prob[0] and the inter frame ones in [1]
static void pack_probs(nvdec_vp9EntropyProbs_t *e, const Vp9Probs *p, const Vp9Frame *f)
{
    memset(e, 0, sizeof *e);
    for (int i = 0; i < 10; i++) for (int j = 0; j < 10; j++) {
        memcpy(e->kf_bmode_prob[i][j], kf_y_mode_probs[i][j], 8); e->kf_bmode_probB[i][j][0] = kf_y_mode_probs[i][j][8];
    }
    for (int i = 0; i < 10; i++) { memcpy(e->kf_uv_mode_prob[i], kf_uv_mode_probs[i], 8); e->kf_uv_mode_probB[i][0] = kf_uv_mode_probs[i][8]; }
    if (f->seg_enabled && f->seg_update_map) { memcpy(e->mb_segment_tree_probs, f->seg_tree_probs, 7); memcpy(e->segment_pred_probs, f->seg_pred_probs, 3); }
    else { memset(e->mb_segment_tree_probs, 255, 7); memset(e->segment_pred_probs, 255, 3); }
    nvdec_vp9AdaptiveEntropyProbs_t *a = &e->a;
    for (int i = 0; i < 7; i++) memcpy(a->inter_mode_prob[i], p->inter_mode[i], 3);
    memcpy(a->intra_inter_prob, p->is_inter, 4);
    for (int i = 0; i < 10; i++) { memcpy(a->uv_mode_prob[i], p->uv_mode[i], 8); a->uv_mode_probB[i][0] = p->uv_mode[i][8]; }
    for (int i = 0; i < 2; i++) {
        a->tx8x8_prob[i][0] = p->tx8[i][0];
        memcpy(a->tx16x16_prob[i], p->tx16[i], 2); memcpy(a->tx32x32_prob[i], p->tx32[i], 3);
    }
    for (int i = 0; i < 4; i++) { memcpy(a->sb_ymode_prob[i], p->y_mode[i], 8); a->sb_ymode_probB[i][0] = p->y_mode[i][8]; }
    for (int i = 0; i < 16; i++) { memcpy(a->partition_prob[0][i], kf_partition_probs[i], 3); memcpy(a->partition_prob[1][i], p->partition[i], 3); }
    memcpy(a->switchable_interp_prob, p->interp_filter, sizeof a->switchable_interp_prob);
    memcpy(a->comp_inter_prob, p->comp_mode, 5);
    memcpy(a->mbskip_probs, p->skip, 3);
    nvdec_nmv_context *m = &a->nmvc;
    memcpy(m->joints, p->mv_joint, 3);
    for (int i = 0; i < 2; i++) {
        m->sign[i] = p->mv_sign[i]; m->class0[i][0] = p->mv_class0_bit[i];
        memcpy(m->fp[i], p->mv_fr[i], 3); m->class0_hp[i] = p->mv_class0_hp[i]; m->hp[i] = p->mv_hp[i];
        memcpy(m->classes[i], p->mv_class[i], 10); memcpy(m->class0_fp[i], p->mv_class0_fr[i], sizeof m->class0_fp[i]);
        memcpy(m->bits[i], p->mv_bits[i], 10);
    }
    memcpy(a->single_ref_prob, p->single_ref, sizeof a->single_ref_prob);
    memcpy(a->comp_ref_prob, p->comp_ref, 5);
    unsigned char (*coef[4])[2][6][6][4] = { a->probCoeffs, a->probCoeffs8x8, a->probCoeffs16x16, a->probCoeffs32x32 };
    for (int t = 0; t < 4; t++) for (int i = 0; i < 2; i++) for (int j = 0; j < 2; j++) for (int k = 0; k < 6; k++) for (int l = 0; l < 6; l++)
        memcpy(coef[t][i][j][k][l], p->coef[t][i][j][k][l], 3);
}

// The engine reads reference samples up to the superblock-aligned size and does not clamp them at the frame edge, which
// the spec does (8.5.2.4: every read is clipped to the last row / column), so the right and bottom padding of each
// reference must repeat the edge (T10.03: the 8x8 and 66x66 test vectors). g = bytes per sample (pair, for chroma).
static void extend_plane(uint8_t *p, size_t pitch, unsigned w, unsigned h, unsigned aw, unsigned ah, unsigned g)
{
    if (aw > w)
        for (unsigned y = 0; y < h; y++) {
            const uint8_t *src = p + nvenc_in_offset((size_t)(w - 1) * g, y, pitch);   // a whole group: g divides 16
            for (size_t x = (size_t)w * g; x < (size_t)aw * g; x++) p[nvenc_in_offset(x, y, pitch)] = src[x % g];
        }
    for (unsigned y = h; y < ah; y++)
        for (size_t x = 0; x < (size_t)aw * g; x += 16) memcpy(p + nvenc_in_offset(x, y, pitch), p + nvenc_in_offset(x, h - 1, pitch), 16);
}

static void extend_edges(Vp9Dec *d, int surf, unsigned w, unsigned h)
{
    const unsigned aw = (w + 63) & ~63u, ah = (h + 63) & ~63u;
    if (aw == w && ah == h) return;
    const unsigned b = (unsigned)d->bps;
    extend_plane(d->luma[surf].cpu, d->pitch, w, h, aw, ah, b);
    extend_plane(d->chroma[surf].cpu, d->pitch, (w + 1) / 2, (h + 1) / 2, aw / 2, ah / 2, 2 * b);
}

static int tile_offset(int n, int mis, int log2) { int sbs = (mis + 7) >> 3, o = ((n * sbs) >> log2) << 3; return o < mis ? o : mis; }

int vp9dec_frame(Vp9Dec *d, const uint8_t *buf, size_t n)
{
    Vp9Frame *f = &d->f;
    if (vp9_parse_frame(&d->st, buf, n, f)) { fprintf(stderr, "vp9dec: header parse failed\n"); return -1; }
    if (f->show_existing_frame) {
        const int s = d->ref[f->frame_to_show_map_idx];
        if (s < 0) { fprintf(stderr, "vp9dec: show_existing_frame of an empty slot\n"); return -1; }
        d->busy[s] = true; d->cur_surf = s; d->cur_w = (int)d->w[s]; d->cur_h = (int)d->h[s]; d->dec_surf = -1;
        return 1;
    }
    if (f->width > 8192 || f->height > 8192) { fprintf(stderr, "vp9dec: %dx%d is beyond NVDEC's VP9 limit (8192)\n", f->width, f->height); return -1; }
    // Turing's NVDEC (GTX 1650, 0xc4b0) answers status 0x10 for frames of one superblock row or column (NVIDIA lists 128x128
    // as VP9's minimum; 66x66 and larger decode bit-exact); Ada decodes down to 8x8
    if (d->turing && (f->width <= 64 || f->height <= 64)) {
        fprintf(stderr, "vp9dec: %dx%d is below this engine's VP9 minimum (65x65)\n", f->width, f->height); return -1; }
    if ((unsigned)f->width > d->maxW || (unsigned)f->height > d->maxH) { fprintf(stderr, "vp9dec: %dx%d is larger than the session\n", f->width, f->height); return -1; }
    if (!f->subsampling_x || !f->subsampling_y) { fprintf(stderr, "vp9dec: only 4:2:0 (NVDEC has no VP9 4:2:2 / 4:4:4)\n"); return -1; }
    if (f->bit_depth != d->bit_depth) { fprintf(stderr, "vp9dec: %d-bit frame in a %d-bit session\n", f->bit_depth, d->bit_depth); return -1; }
    int surf = -1;
    for (int i = 0; i < VP9_SURFACES && surf < 0; i++) {
        bool used = d->busy[i];
        for (int k = 0; k < 8; k++) if (d->ref[k] == i) used = true;
        if (!used) surf = i;
    }
    if (surf < 0) { fprintf(stderr, "vp9dec: no free surface\n"); return -1; }
    const bool size_changed = !d->have_last || f->width != d->last_w || f->height != d->last_h;
    // the segmentation map read for prediction is cleared on a size change (7.2.6) and by setup_past_independence (7.2)
    if (size_changed || f->frame_is_intra || f->error_resilient_mode) memset(d->seg[d->seg_prev].cpu, 0, d->seg[d->seg_prev].size);

    // bitstream: the tile data (after both headers)
    const size_t hdr = (size_t)f->uncomp_bytes + (size_t)f->header_size_in_bytes, len = n - hdr;
    if (len + 64 > d->bs.size) { fprintf(stderr, "vp9dec: frame too large\n"); return -1; }
    memcpy(d->bs.cpu, buf + hdr, len); memset((uint8_t *)d->bs.cpu + len, 0, 64);
    pack_probs(d->probs.cpu, &d->st.fc, f);
    memset(d->counts.cpu, 0, d->counts.size);
    // tile sizes: width and height in superblocks, 2 bytes each, tiles in raster order (GIP_ASIC_TILE_SIZE)
    const int mi_cols = (f->width + 7) >> 3, mi_rows = (f->height + 7) >> 3;
    uint16_t *ts = d->tiles.cpu; int nt = 0;
    for (int r = 0; r < 1 << f->tile_rows_log2; r++) for (int c = 0; c < 1 << f->tile_cols_log2; c++) {
        const int c0 = tile_offset(c, mi_cols, f->tile_cols_log2), c1 = tile_offset(c + 1, mi_cols, f->tile_cols_log2);
        const int r0 = tile_offset(r, mi_rows, f->tile_rows_log2), r1 = tile_offset(r + 1, mi_rows, f->tile_rows_log2);
        ts[2 * nt] = (uint16_t)((c1 - c0 + 7) >> 3); ts[2 * nt + 1] = (uint16_t)((r1 - r0 + 7) >> 3); nt++;
    }
    // references: LAST / GOLDEN / ALTREF surfaces and sizes (key and intra-only frames: the current surface)
    int rs[3];
    for (int i = 0; i < 3; i++) { rs[i] = f->frame_is_intra ? -1 : d->ref[f->ref_frame_idx[i]]; if (rs[i] < 0) rs[i] = surf; }

    nvdec_vp9_pic_s *ps = d->setup.cpu; memset(ps, 0, sizeof *ps);
    ps->stream_len = (unsigned)len;
    ps->tileformat = 1; ps->gob_height = 0;
    ps->Vp9BsdCtrlOffset = (unsigned)(d->flt_bsd >> 8);
    unsigned short *rw[3] = { &ps->ref0_width, &ps->ref1_width, &ps->ref2_width };
    for (int i = 0; i < 3; i++) {
        unsigned short *r = rw[i];
        r[0] = (unsigned short)(rs[i] == surf ? f->width : (int)d->w[rs[i]]); r[1] = (unsigned short)(rs[i] == surf ? f->height : (int)d->h[rs[i]]);
        r[2] = r[3] = (unsigned short)(d->pitch / (size_t)d->bps);   // strides in samples (T10.03: 10-bit rows landed 2x apart)
    }
    ps->width = (unsigned short)f->width; ps->height = (unsigned short)f->height;
    ps->framestride[0] = ps->framestride[1] = (unsigned short)(d->pitch / (size_t)d->bps);
    ps->keyFrame = f->frame_type == VP9_KEY_FRAME;
    ps->prevIsKeyFrame = f->last_frame_type == VP9_KEY_FRAME;
    ps->resolutionChange = size_changed;
    ps->errorResilient = (unsigned char)f->error_resilient_mode;
    ps->prevShowFrame = (unsigned char)(d->have_last && d->last_show);
    ps->intraOnly = (unsigned char)f->intra_only;
    for (int i = 0; i < 4; i++) ps->refFrameSignBias[i] = (unsigned char)f->ref_frame_sign_bias[i];
    ps->loopFilterLevel = (char)f->lf_level; ps->loopFilterSharpness = (char)f->lf_sharpness;
    ps->qpYAc = (unsigned char)f->base_q_idx; ps->qpYDc = (char)f->delta_q_y_dc; ps->qpChAc = (char)f->delta_q_uv_ac; ps->qpChDc = (char)f->delta_q_uv_dc;
    ps->lossless = (char)f->lossless; ps->transform_mode = (char)f->tx_mode;
    ps->allow_high_precision_mv = (char)f->allow_high_precision_mv; // the engine numbers the filters in the bitstream's literal order (smooth, regular, sharp, bilinear; 4 = switchable),
    // not the spec's enum (T10.02: regular frames came out as smooth)
    static const char filter_literal[5] = { 1, 0, 2, 3, 4 };
    ps->mcomp_filter_type = filter_literal[f->interp_filter];
    ps->comp_pred_mode = (char)f->reference_mode; ps->comp_fixed_ref = (char)f->comp_fixed_ref;
    ps->comp_var_ref[0] = (char)f->comp_var_ref[0]; ps->comp_var_ref[1] = (char)f->comp_var_ref[1];
    ps->log2_tile_columns = (char)f->tile_cols_log2; ps->log2_tile_rows = (char)f->tile_rows_log2;
    ps->segmentEnabled = (unsigned char)f->seg_enabled; ps->segmentMapUpdate = (unsigned char)f->seg_update_map;
    ps->segmentMapTemporalUpdate = (unsigned char)f->seg_temporal_update; ps->segmentFeatureMode = (unsigned char)d->st.seg_abs_or_delta_update;
    for (int i = 0; i < 8; i++) for (int j = 0; j < 4; j++) {
        ps->segmentFeatureEnable[i][j] = (unsigned char)d->st.seg_feature_enabled[i][j];
        ps->segmentFeatureData[i][j] = (short)d->st.seg_feature_data[i][j];
    }
    ps->modeRefLfEnabled = (char)d->st.lf_delta_enabled;
    for (int i = 0; i < 4; i++) ps->mbRefLfDelta[i] = (char)d->st.lf_ref_deltas[i];
    for (int i = 0; i < 2; i++) ps->mbModeLfDelta[i] = (char)d->st.lf_mode_deltas[i];
    ps->v1.Vp9FltAboveOffset = (unsigned)(d->flt_above >> 8);
    ps->v1.bit_depth = (unsigned)f->bit_depth;

    Chan *c = &d->chan;
    chan_rewind(c);
    chan_mthd(c, 0, NVC9B0_SET_APPLICATION_ID, 1, (NvU32)NVC9B0_SET_APPLICATION_ID_ID_VP9);
    chan_mthd(c, 0, NVC9B0_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _CODEC_TYPE, NVDEC_CODEC_VP9) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _GPTIMER_ON, 1));
    chan_mthd(c, 0, NVC9B0_SET_DRV_PIC_SETUP_OFFSET, 1, hi(d->setup.gpu));
    chan_mthd(c, 0, NVC9B0_SET_IN_BUF_BASE_OFFSET, 1, hi(d->bs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_PICTURE_INDEX, 1, (NvU32)VP9_CURR_FRAME_SFC);
    chan_mthd(c, 0, NVC9B0_SET_NVDEC_STATUS_OFFSET, 1, hi(d->status.gpu));
    chan_mthd(c, 0, NVC9B0_VP9_SET_PROB_TAB_BUF_OFFSET, 1, hi(d->probs.gpu));
    chan_mthd(c, 0, NVC9B0_VP9_SET_CTX_COUNTER_BUF_OFFSET, 1, hi(d->counts.gpu));
    chan_mthd(c, 0, NVC9B0_VP9_SET_SEGMENT_READ_BUF_OFFSET, 1, hi(d->seg[d->seg_prev].gpu));
    chan_mthd(c, 0, NVC9B0_VP9_SET_SEGMENT_WRITE_BUF_OFFSET, 1, hi(d->seg[d->seg_prev ^ 1].gpu));
    chan_mthd(c, 0, NVC9B0_VP9_SET_TILE_SIZE_BUF_OFFSET, 1, hi(d->tiles.gpu));
    chan_mthd(c, 0, NVC9B0_VP9_SET_COL_MVWRITE_BUF_OFFSET, 1, hi(d->colmv[d->mv_prev ^ 1].gpu));
    chan_mthd(c, 0, NVC9B0_VP9_SET_COL_MVREAD_BUF_OFFSET, 1, hi(d->colmv[d->mv_prev].gpu));
    chan_mthd(c, 0, NVC9B0_VP9_SET_FILTER_BUFFER_OFFSET, 1, hi(d->filter.gpu));
    // Ada: without it an MMU fault at address 0 (T10.02). Turing's class (0xc4b0) has no such method and NVIDIA's Linux driver
    // sends none (docs/traces/vp9dec-gtx1650-linux); given one, that engine used the buffer as 32 bytes per superblock, decoded
    // frames that keep the segmentation map as if segmentation were off and faulted when the buffer was smaller than that
    if (!d->turing) chan_mthd(c, 0, NVC9B0_SET_INTRA_TOP_BUF_OFFSET, 1, hi(d->intra.gpu));
    const int sfc[4] = { rs[0], rs[1], rs[2], surf };          // VP9_LAST/GOLDEN/ALTREF/CURR_FRAME_SFC
    for (int i = 0; i < 4; i++) {
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_LUMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->luma[sfc[i]].gpu));
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_CHROMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->chroma[sfc[i]].gpu));
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
    if (sv[0] != val) { fprintf(stderr, "vp9dec: no completion (error notifier 0x%x)\n", chan_error(c)); return -1; }
    const nvdec_status_s *st = d->status.cpu;
    d->err_status = st->error_status; d->ok_blocks = st->mbs_correctly_decoded;
    if (d->verbose)
        printf("  vp9dec: surf %2d %s%s %dx%d q %d refs %d,%d,%d -> status 0x%x, ok %u, err %u, intra %u inter %u, %.2f ms\n", surf,
               f->frame_type == VP9_KEY_FRAME ? "key" : f->intra_only ? "intra" : "inter", f->show_frame ? "" : " (hidden)",
               f->width, f->height, f->base_q_idx, rs[0], rs[1], rs[2], st->error_status, st->mbs_correctly_decoded, st->mbs_in_error,
               st->vp9.frame_status_intra_cnt, st->vp9.frame_status_inter_cnt, d->engine_ms);

    if (f->refresh_frame_flags) extend_edges(d, surf, (unsigned)f->width, (unsigned)f->height);
    // after the frame: adaptation, reference slots, segmentation map, column MVs (6.1.2, 8.1, 8.10)
    vp9_end_frame(&d->st, f, vp9_adapts(f) ? d->counts.cpu : NULL);
    for (int i = 0; i < 8; i++) if (f->refresh_frame_flags & (1 << i)) d->ref[i] = surf;
    d->w[surf] = (unsigned)f->width; d->h[surf] = (unsigned)f->height;
    if (f->seg_enabled && f->seg_update_map) d->seg_prev ^= 1;
    d->mv_prev ^= 1;
    d->have_last = true; d->last_w = f->width; d->last_h = f->height; d->last_show = f->show_frame; d->dec_surf = surf;
    if (!f->show_frame) return 0;
    d->busy[surf] = true; d->cur_surf = surf; d->cur_w = f->width; d->cur_h = f->height;
    return 1;
}
