// hevcdec.c: see hevcdec.h. Section numbers refer to ITU-T H.265.
#include "hevcdec.h"
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

int hevcdec_open(HevcDec *d, Rm *rm, unsigned maxW, unsigned maxH, int bit_depth, int chroma_format)
{
    memset(d, 0, sizeof *d);
    // Ada's NVDEC: 4:2:0 / 4:4:4, 8-12 bit, up to 8192x8192 (8440 lines decode as garbage); 4:2:2 (NVIDIA lists it from
    // Blackwell on) and monochrome are answered with status 0x5
    if (chroma_format == 0 || chroma_format == 2 || bit_depth > 12 || maxW > 8192 || maxH > 8192) {
        fprintf(stderr, "hevcdec: %s is not supported by this engine\n", chroma_format == 0 ? "monochrome" : chroma_format == 2 ? "4:2:2" :
                bit_depth > 12 ? "bit depth > 12" : "a picture over 8192");
        return -1;
    }
    d->rm = rm; d->maxW = (maxW + 63) & ~63u; d->maxH = (maxH + 63) & ~63u;
    d->bit_depth = bit_depth; d->bps = bit_depth > 8 ? 2 : 1; d->chroma_format = chroma_format;
    d->pitch = (size_t)d->maxW * (size_t)d->bps;
    d->first_pic = true; d->after_eos = true;
    NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = 0 };
    d->turing = rm_class(rm, RM_CLS_NVDEC) == 0xc4b0;
    if (chan_create(rm, &d->chan, NV2080_ENGINE_TYPE_NVDEC0, rm_class(rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "hevcdec: channel failed\n"); return -1; }
    const size_t b16 = (size_t)(d->maxW / 16) * (d->maxH / 16);
    // co-located MVs: 128 bytes per 16x16 block per picture (generous; the headers give no size)
    d->colmv_size = NVDEC_ALIGN(b16 * 128);
    // filter buffer: deblocking top + left lines and the SAO left lines (nvdec_drv.h's sizes), then the BSD control, filter
    // above and SAO above areas, sized generously (per row, for up to 24 tile columns), then the slice edge area
    const size_t top = HEVC_DBLK_TOP_BUF_SIZE(d->maxW), left = HEVC_DBLK_LEFT_BUF_SIZE(d->maxH);
    d->flt_sao = NVDEC_ALIGN(top + left);
    // the SAO left area 8x nvdec_drv.h's size: Turing's NVDEC (GTX 1650, 0xc4b0) needs more with 3+ tile columns and rows
    // (5x5 tiles at 1080p: 2x; the 1x size ran into the BSD control area: decoding stopped part way, wrong tile edges)
    d->flt_bsd = d->flt_sao + NVDEC_ALIGN(HEVC_SAO_LEFT_BUF_SIZE(d->maxH) * 8);
    d->flt_above = d->flt_bsd + NVDEC_ALIGN((size_t)d->maxH * GIP_ASIC_BSD_CTRL_RAM_SIZE * 24);
    // the filter-above and SAO-above areas by the larger picture dimension: Turing's NVDEC splits pictures wider than ~3072
    // into columns and fills them by width; sized by the height they overlapped (wrong rows at CTB edges)
    const size_t dimA = (size_t)(d->maxW > d->maxH ? d->maxW : d->maxH);
    d->flt_sao_above = d->flt_above + NVDEC_ALIGN(dimA * GIP_ASIC_VERT_FILTER_RAM_SIZE * 24);
    d->flt_edge = d->flt_sao_above + NVDEC_ALIGN(dimA * GIP_ASIC_VERT_SAO_RAM_SIZE * 24);
    size_t flt = d->flt_edge + NVDEC_ALIGN((size_t)d->maxW * 64);
    // Turing's NVDEC writes one stretch from the filter-above area on, through the SAO-above and edge areas, that grows with the
    // picture area: 0.375 bytes per pixel at 8-bit 4:2:0, 0.5 at 10-bit or 8-bit 4:4:4, 0.68 at 12-bit 4:4:4 (T15.11; pictures
    // over ~9 Mpixel, e.g. 4096x2304 or 8K, overran the buffer: no completion); give it half a byte per pixel and sample byte,
    // twice that for 4:4:4
    const size_t turingA = (size_t)d->maxW * d->maxH * d->bps * (chroma_format == 3 ? 2 : 1) / 2;
    if (d->turing && flt < d->flt_above + turingA) flt = d->flt_above + NVDEC_ALIGN(turingA);
    d->auCap = (size_t)d->maxW * d->maxH * 2 > (4u << 20) ? (size_t)d->maxW * d->maxH * 2 : (4u << 20);
    if (!(d->au = malloc(d->auCap))) return -1;
    struct { RmMem *m; size_t size; } b[] = {
        { &d->setup, 0x2000 }, { &d->bs, d->auCap + 0x1000 }, { &d->scaling, 0x1000 }, { &d->tiles, 0x1000 },
        { &d->filter, flt }, { &d->coloc, d->colmv_size * HEVC_SURFACES }, { &d->intra, (size_t)d->maxW * 8 * (size_t)d->bps },
        { &d->slinfo, 0x10000 }, { &d->status, 0x1000 }, { &d->sem, 0x1000 } };
    for (size_t i = 0; i < sizeof b / sizeof b[0]; i++) {
        if (rm_mem_alloc(rm, b[i].m, (b[i].size + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "hevcdec: alloc failed\n"); return -1; }
        memset(b[i].m->cpu, 0, b[i].m->size);
    }
    const size_t ch = chroma_format == 1 ? d->maxH / 2 : d->maxH;
    const size_t luma = nvenc_in_plane_size(d->pitch, d->maxH) + 65536, chroma = nvenc_in_plane_size(d->pitch * (chroma_format == 3 ? 2 : 1), ch) + 65536;
    for (int i = 0; i < HEVC_SURFACES; i++)
        if (rm_mem_alloc(rm, &d->luma[i], (luma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &d->chroma[i], (chroma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "hevcdec: surface alloc failed\n"); return -1; }
    chan_mthd(&d->chan, 0, NVC56F_SET_OBJECT, 1, rm_class(rm, RM_CLS_NVDEC));
    return 0;
}

void hevcdec_close(HevcDec *d)
{
    chan_destroy(&d->chan);
    RmMem *m[] = { &d->setup, &d->bs, &d->scaling, &d->tiles, &d->filter, &d->coloc, &d->intra, &d->slinfo, &d->status, &d->sem };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) rm_mem_free(d->rm, m[i]);
    for (int i = 0; i < HEVC_SURFACES; i++) { rm_mem_free(d->rm, &d->luma[i]); rm_mem_free(d->rm, &d->chroma[i]); }
    free(d->au); d->au = NULL;
}

void hevcdec_release(HevcDec *d, int surf) { if (surf >= 0 && surf < HEVC_SURFACES) d->busy[surf] = false; }

static unsigned sample(const uint8_t *p, size_t x, size_t y, size_t pitch, int bps, int shift)
{
    if (bps == 1) return p[nvenc_in_offset(x, y, pitch)];
    const size_t o = nvenc_in_offset(2 * x, y, pitch);
    return (unsigned)(p[o] | p[o + 1] << 8) >> shift;
}

void hevcdec_read_nv12(HevcDec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h, unsigned x0, unsigned y0)
{
    const size_t b = (size_t)d->bps, cp = d->chroma_format == 3 ? 2 * d->pitch : d->pitch;
    const unsigned cw = d->chroma_format == 3 ? w : (w + 1) / 2, ch = d->chroma_format == 3 ? h : (h + 1) / 2;
    if (!x0 && !y0) {
        nvenc_in_untile_plane(y, ystride, d->luma[surf].cpu, w * b, h, d->pitch);
        nvenc_in_untile_plane(uv, uvstride, d->chroma[surf].cpu, 2 * cw * b, ch, cp);
        return;
    }
    // a conformance window that crops the left / top: sample by sample (rare)
    const unsigned cx0 = d->chroma_format == 3 ? x0 : x0 / 2, cy0 = d->chroma_format == 3 ? y0 : y0 / 2;
    for (unsigned r = 0; r < h; r++) for (unsigned x = 0; x < w; x++) for (size_t k = 0; k < b; k++)
        y[r * ystride + x * b + k] = ((const uint8_t *)d->luma[surf].cpu)[nvenc_in_offset((x0 + x) * b + k, y0 + r, d->pitch)];
    for (unsigned r = 0; r < ch; r++) for (unsigned x = 0; x < 2 * cw; x++) for (size_t k = 0; k < b; k++)
        uv[r * uvstride + x * b + k] = ((const uint8_t *)d->chroma[surf].cpu)[nvenc_in_offset((2 * cx0 + x) * b + k, cy0 + r, cp)];
}

void hevcdec_read_yuv(HevcDec *d, int surf, uint8_t *dst, const int crop[4], unsigned w, unsigned h)
{
    const uint8_t *L = d->luma[surf].cpu, *C = d->chroma[surf].cpu;
    const int sx = d->chroma_format == 3 ? 0 : 1, sy = d->chroma_format == 1 ? 1 : 0;
    const unsigned cw = (w + (unsigned)sx) >> sx, ch = (h + (unsigned)sy) >> sy, x0 = (unsigned)crop[0], y0 = (unsigned)crop[2];
    const int b = d->bps, sh = b == 2 ? 16 - d->bit_depth : 0;   // 10/12-bit samples MSB-aligned (like P010)
    #define PUT(i, v) do { if (b == 1) dst[i] = (uint8_t)(v); else { dst[2 * (i)] = (uint8_t)(v); dst[2 * (i) + 1] = (uint8_t)((v) >> 8); } } while (0)
    for (unsigned y = 0; y < h; y++) for (unsigned x = 0; x < w; x++) PUT((size_t)y * w + x, sample(L, x0 + x, y0 + y, d->pitch, b, sh));
    const size_t uo = (size_t)w * h, vo = uo + (size_t)cw * ch;
    for (unsigned y = 0; y < ch; y++) for (unsigned x = 0; x < cw; x++) {
        const size_t cx = (x0 >> sx) + x, cy = (y0 >> sy) + y;
        PUT(uo + (size_t)y * cw + x, sample(C, 2 * cx, cy, d->pitch * (d->chroma_format == 3 ? 2 : 1), b, sh));
        PUT(vo + (size_t)y * cw + x, sample(C, 2 * cx + 1, cy, d->pitch * (d->chroma_format == 3 ? 2 : 1), b, sh));
    }
    #undef PUT
}

// ---- scaling lists into the engine's buffer (GIP_ASIC_SCALING_LIST_SIZE: "8 dc + 8 to boundary + 6*16 + 2*6*64 + 2*64"):
// the 16x16 then 32x32 DC values, 8 bytes padding, then the 4x4, 8x8, 16x16 and 32x32 (intra and inter luma) lists, each
// column by column (transposed raster; found with the conformance SLIST streams, T11.03)
static void diag_scan(int n, uint8_t *xs, uint8_t *ys)        // 6.5.3: up-right diagonal scan of an n x n block
{
    int i = 0, x = 0, y = 0; bool stop = false;
    while (!stop) {
        while (y >= 0) { if (x < n && y < n) { xs[i] = (uint8_t)x; ys[i] = (uint8_t)y; i++; } y--; x++; }
        y = x; x = 0;
        if (i >= n * n) stop = true;
    }
}
static void write_scaling(uint8_t *p, const HevcScaling *sl)
{
    uint8_t x4[16], y4[16], x8[64], y8[64];
    diag_scan(4, x4, y4); diag_scan(8, x8, y8);
    memset(p, 0, GIP_ASIC_SCALING_LIST_SIZE);
    for (int m = 0; m < 6; m++) p[m] = sl->dc[0][m];
    p[6] = sl->dc[1][0]; p[7] = sl->dc[1][3];
    uint8_t *q = p + 16;
    for (int size = 0; size < 4; size++) {
        const int n = size == 0 ? 4 : 8;
        const uint8_t *xs = n == 4 ? x4 : x8, *ys = n == 4 ? y4 : y8;
        for (int m = 0; m < 6; m += size == 3 ? 3 : 1) {
            for (int i = 0; i < n * n; i++) q[xs[i] * n + ys[i]] = sl->list[size][m][i];
            q += n * n;
        }
    }
}

// ---- reference picture set (8.3.2) ----
typedef struct { int st_before[16], st_after[16], lt_curr[16], n_before, n_after, n_lt, foll[32], nfoll; } Rps;

static int find_poc(const HevcDec *d, int poc, bool lsb_only, int max_lsb)
{
    for (int i = 0; i < HEVC_SURFACES; i++)
        if (d->ref[i] && (lsb_only ? (d->poc[i] & (max_lsb - 1)) == poc : d->poc[i] == poc)) return i;
    return -1;
}

static int apply_rps(HevcDec *d, const HevcSps *sps, const HevcSlice *s, int poc, bool irap_nro, Rps *o)
{
    memset(o, 0, sizeof *o);
    if (hevc_is_irap(s->nal_type) && irap_nro) for (int i = 0; i < HEVC_SURFACES; i++) d->ref[i] = 0;
    if (hevc_is_idr(s->nal_type)) return 0;
    const int max_lsb = 1 << sps->log2_max_poc_lsb;
    bool keep[HEVC_SURFACES] = { false }; bool lt[HEVC_SURFACES] = { false };
    int missing = 0;
    // long-term first (they may also match a short-term POC)
    int msb_cycle = 0;
    for (int i = 0; i < s->num_long_term_sps + s->num_long_term_pics; i++) {
        if (i == 0 || i == s->num_long_term_sps) msb_cycle = s->lt_delta_msb_cycle[i]; else msb_cycle += s->lt_delta_msb_cycle[i];
        int p = s->lt_poc_lsb[i]; bool lsb_only = !s->lt_msb_present[i];
        if (!lsb_only) p += poc - msb_cycle * max_lsb - (poc & (max_lsb - 1));
        const int k = find_poc(d, p, lsb_only, max_lsb);
        if (k < 0) { missing++; continue; }
        keep[k] = lt[k] = true;
        if (s->lt_used[i]) o->lt_curr[o->n_lt++] = k; else o->foll[o->nfoll++] = k;
    }
    for (int i = 0; i < s->st_rps.num_negative + s->st_rps.num_positive; i++) {
        const int k = find_poc(d, poc + s->st_rps.delta_poc[i], false, max_lsb);
        if (k < 0 || lt[k]) { if (s->st_rps.used[i]) missing++; continue; }
        keep[k] = true;
        if (!s->st_rps.used[i]) o->foll[o->nfoll++] = k;
        else if (i < s->st_rps.num_negative) o->st_before[o->n_before++] = k;
        else o->st_after[o->n_after++] = k;
    }
    for (int i = 0; i < HEVC_SURFACES; i++) d->ref[i] = keep[i] ? (lt[i] ? 2 : 1) : 0;
    return missing;
}

// ---- one picture ----
static int decode_pending(HevcDec *d)
{
    if (!d->auSlices) return 0;
    const int nAu = d->auSlices; d->auSlices = 0;
    const HevcSlice *sl = &d->auFirst;
    const HevcPps *pps = &d->params.pps[sl->pps_id]; const HevcSps *sps = &d->params.sps[pps->sps_id];
    if ((unsigned)sps->width > d->maxW || (unsigned)sps->height > d->maxH) { fprintf(stderr, "hevcdec: %dx%d is larger than the session (%ux%u)\n", sps->width, sps->height, d->maxW, d->maxH); return -1; }
    if (sps->bit_depth_luma != d->bit_depth || sps->chroma_format_idc != d->chroma_format) { fprintf(stderr, "hevcdec: format changed\n"); return -1; }
    const int t = sl->nal_type;
    const bool irap = hevc_is_irap(t);
    bool nro = false;
    if (irap) { nro = hevc_is_idr(t) || (t >= HEVC_NAL_BLA_W_LP && t <= 18) || d->first_pic || d->after_eos; d->skip_rasl = nro; }
    const bool cra_after_eos = t == HEVC_NAL_CRA && d->after_eos && !d->first_pic;
    d->first_pic = false; d->after_eos = false;
    if ((t == 8 || t == 9) && d->skip_rasl) { d->auN = 0; return 0; }   // RASL after a CRA that starts decoding: not decodable (8.1.3)
    const int poc = hevc_poc(&d->pocst, sps, sl, nro);
    Rps r; const int missing = apply_rps(d, sps, sl, poc, irap && nro, &r);
    if (missing && d->verbose) printf("  hevcdec: POC %d: %d reference pictures missing\n", poc, missing);
    // a free surface: not held by the caller, not a reference
    int surf = -1;
    for (int i = 0; i < HEVC_SURFACES && surf < 0; i++) if (!d->busy[i] && !d->ref[i]) surf = i;
    if (surf < 0) { fprintf(stderr, "hevcdec: no free surface (the caller holds too many)\n"); return -1; }

    memcpy(d->bs.cpu, d->au, d->auN);
    const int ctb = 1 << sps->log2_max_cb;
    nvdec_hevc_pic_s *ps = d->setup.cpu; memset(ps, 0, sizeof *ps);
    ps->stream_len = (unsigned)d->auN;
    ps->tileformat = 1; ps->gob_height = 0;
    ps->sw_start_code_e = 1;
    // in samples, as VP9's (T10.03); 4:4:4 chroma is Cb/Cr interleaved at full width (like NV24), twice the luma stride
    ps->framestride[0] = (unsigned)(d->pitch / (size_t)d->bps);
    ps->framestride[1] = ps->framestride[0] * (sps->chroma_format_idc == 3 ? 2 : 1);
    ps->colMvBuffersize = (unsigned)(d->colmv_size >> 8);
    ps->HevcSaoBufferOffset = (unsigned)(d->flt_sao >> 8); ps->HevcBsdCtrlOffset = (unsigned)(d->flt_bsd >> 8);
    ps->pic_width_in_luma_samples = (unsigned short)sps->width; ps->pic_height_in_luma_samples = (unsigned short)sps->height;
    ps->chroma_format_idc = (unsigned)sps->chroma_format_idc;
    ps->bit_depth_luma = (unsigned)sps->bit_depth_luma; ps->bit_depth_chroma = (unsigned)sps->bit_depth_chroma;
    ps->log2_min_luma_coding_block_size = (unsigned)sps->log2_min_cb; ps->log2_max_luma_coding_block_size = (unsigned)sps->log2_max_cb;
    ps->log2_min_transform_block_size = (unsigned)sps->log2_min_tb; ps->log2_max_transform_block_size = (unsigned)sps->log2_max_tb;
    ps->max_transform_hierarchy_depth_inter = (unsigned)sps->max_th_depth_inter; ps->max_transform_hierarchy_depth_intra = (unsigned)sps->max_th_depth_intra;
    ps->scalingListEnable = (unsigned)sps->scaling_list_enabled; ps->amp_enable_flag = (unsigned)sps->amp_enabled;
    ps->sample_adaptive_offset_enabled_flag = (unsigned)sps->sao_enabled; ps->pcm_enabled_flag = (unsigned)sps->pcm_enabled;
    if (sps->pcm_enabled) {
        ps->pcm_sample_bit_depth_luma = (unsigned)sps->pcm_bit_depth_luma; ps->pcm_sample_bit_depth_chroma = (unsigned)sps->pcm_bit_depth_chroma;
        ps->log2_min_pcm_luma_coding_block_size = (unsigned)sps->log2_min_pcm_cb; ps->log2_max_pcm_luma_coding_block_size = (unsigned)sps->log2_max_pcm_cb;
        ps->pcm_loop_filter_disabled_flag = (unsigned)sps->pcm_loop_filter_disabled;
    }
    ps->sps_temporal_mvp_enabled_flag = (unsigned)sps->temporal_mvp_enabled;
    ps->strong_intra_smoothing_enabled_flag = (unsigned)sps->strong_intra_smoothing_enabled;
    ps->dependent_slice_segments_enabled_flag = (unsigned)pps->dependent_slice_segments_enabled;
    ps->output_flag_present_flag = (unsigned)pps->output_flag_present;
    ps->num_extra_slice_header_bits = (unsigned)pps->num_extra_slice_header_bits;
    ps->sign_data_hiding_enabled_flag = (unsigned)pps->sign_data_hiding_enabled;
    ps->cabac_init_present_flag = (unsigned)pps->cabac_init_present;
    ps->num_ref_idx_l0_default_active = (unsigned)pps->num_ref_idx_l0_default_active;
    ps->num_ref_idx_l1_default_active = (unsigned)pps->num_ref_idx_l1_default_active;
    ps->init_qp = (unsigned)(26 + pps->init_qp_minus26 + 6 * (sps->bit_depth_luma - 8)) & 127;   // with QpBdOffsetY: 0..127 (a guess)
    ps->constrained_intra_pred_flag = (unsigned)pps->constrained_intra_pred;
    ps->transform_skip_enabled_flag = (unsigned)pps->transform_skip_enabled;
    ps->cu_qp_delta_enabled_flag = (unsigned)pps->cu_qp_delta_enabled; ps->diff_cu_qp_delta_depth = (unsigned)pps->diff_cu_qp_delta_depth;
    ps->pps_cb_qp_offset = (char)pps->cb_qp_offset; ps->pps_cr_qp_offset = (char)pps->cr_qp_offset;
    ps->pps_beta_offset = (char)(2 * pps->beta_offset_div2); ps->pps_tc_offset = (char)(2 * pps->tc_offset_div2);
    ps->pps_slice_chroma_qp_offsets_present_flag = (unsigned)pps->slice_chroma_qp_offsets_present;
    ps->weighted_pred_flag = (unsigned)pps->weighted_pred; ps->weighted_bipred_flag = (unsigned)pps->weighted_bipred;
    ps->transquant_bypass_enabled_flag = (unsigned)pps->transquant_bypass_enabled;
    ps->tiles_enabled_flag = (unsigned)pps->tiles_enabled; ps->entropy_coding_sync_enabled_flag = (unsigned)pps->entropy_coding_sync_enabled;
    ps->num_tile_columns = (unsigned)pps->num_tile_columns; ps->num_tile_rows = (unsigned)pps->num_tile_rows;
    ps->loop_filter_across_tiles_enabled_flag = (unsigned)pps->loop_filter_across_tiles;
    ps->loop_filter_across_slices_enabled_flag = (unsigned)pps->loop_filter_across_slices;
    ps->deblocking_filter_control_present_flag = (unsigned)pps->deblocking_filter_control_present;
    ps->deblocking_filter_override_enabled_flag = (unsigned)pps->deblocking_filter_override_enabled;
    ps->pps_deblocking_filter_disabled_flag = (unsigned)pps->pps_deblocking_filter_disabled;
    ps->lists_modification_present_flag = (unsigned)pps->lists_modification_present;
    ps->log2_parallel_merge_level = (unsigned)pps->log2_parallel_merge_level;
    ps->slice_segment_header_extension_present_flag = (unsigned)pps->slice_segment_header_extension_present;
    // references: picture slot = surface; the initial lists (RefPicListTemp0/1, 8.3.4) repeat the RPS to 16 entries
    int nref = 0;
    for (int i = 0; i < HEVC_SURFACES; i++) if (d->ref[i]) {
        nref++;
        int diff = poc - d->poc[i]; diff = diff < -128 ? -128 : diff > 127 ? 127 : diff;
        ps->RefDiffPicOrderCnts[i] = (short)diff;
        if (d->ref[i] == 2) ps->longtermflag |= (unsigned short)(1u << (15 - i));
    }
    // NumPicTotalCurr, not every reference: the engine sizes list_entry_lX from it (LTRPSPS_A: a long-term picture kept for
    // later pictures made a list modification misparse, status 0x5)
    const int total = r.n_before + r.n_after + r.n_lt;
    ps->num_ref_frames = (unsigned char)total;
    if (total) for (int i = 0; i < 16; i++) {
        int k = i % total;
        ps->initreflistidxl0[i] = (unsigned char)(k < r.n_before ? r.st_before[k] : k < r.n_before + r.n_after ? r.st_after[k - r.n_before] : r.lt_curr[k - r.n_before - r.n_after]);
        ps->initreflistidxl1[i] = (unsigned char)(k < r.n_after ? r.st_after[k] : k < r.n_after + r.n_before ? r.st_before[k - r.n_after] : r.lt_curr[k - r.n_before - r.n_after]);
    }
    ps->IDR_picture_flag = hevc_is_idr(t); ps->RAP_picture_flag = irap;
    ps->curr_pic_idx = (unsigned char)surf;
    // the slice header bits the engine skips (found by sweeps, T11.03): everything after slice_type up to the end of the
    // reference picture set: pic_output_flag, colour_plane_id, slice_pic_order_cnt_lsb, short_term_ref_pic_set_sps_flag
    // to the end of the long-term part. The driver has applied them (OPFLAG_A failed with status 0x5 until pic_output_flag
    // was counted, also on IDR pictures)
    ps->sw_hdr_skip_length = (unsigned short)((pps->output_flag_present ? 1 : 0) + (sps->separate_colour_plane_flag ? 2 : 0) +
                                              (hevc_is_idr(t) ? 0 : sps->log2_max_poc_lsb + sl->rps_bits));
    ps->separate_colour_plane_flag = (unsigned)sps->separate_colour_plane_flag;
    ps->log2_max_pic_order_cnt_lsb_minus4 = (unsigned)sps->log2_max_poc_lsb - 4;
    ps->num_short_term_ref_pic_sets = (unsigned)sps->num_short_term_ref_pic_sets;
    ps->num_long_term_ref_pics_sps = (unsigned)sps->num_long_term_ref_pics_sps;
    ps->num_delta_pocs_of_rps_idx = (unsigned)sl->st_rps_ref_num_delta;
    ps->long_term_ref_pics_present_flag = (unsigned)sps->long_term_ref_pics_present;
    ps->num_bits_short_term_ref_pics_in_slice = (unsigned)sl->st_rps_bits;
    nvdec_hevc_main10_444_ext_s *x = &ps->v1.hevc_main10_444_ext;
    x->transformSkipRotationEnableFlag = (unsigned)sps->transform_skip_rotation_enabled;
    x->transformSkipContextEnableFlag = (unsigned)sps->transform_skip_context_enabled;
    x->implicitRdpcmEnableFlag = (unsigned)sps->implicit_rdpcm_enabled; x->explicitRdpcmEnableFlag = (unsigned)sps->explicit_rdpcm_enabled;
    x->extendedPrecisionProcessingFlag = (unsigned)sps->extended_precision_processing;
    x->intraSmoothingDisabledFlag = (unsigned)sps->intra_smoothing_disabled;
    x->highPrecisionOffsetsEnableFlag = (unsigned)sps->high_precision_offsets_enabled;
    x->fastRiceAdaptationEnableFlag = (unsigned)sps->persistent_rice_adaptation_enabled;
    x->cabacBypassAlignmentEnableFlag = (unsigned)sps->cabac_bypass_alignment_enabled;
    // the full log2 size, not "_minus2" as the header's comment says (T11.03: 4x4 transform skip needs 2, else status 0x5)
    if (pps->transform_skip_enabled) x->log2MaxTransformSkipSize = (unsigned)(pps->log2_max_transform_skip_size ? pps->log2_max_transform_skip_size : 2);
    x->crossComponentPredictionEnableFlag = (unsigned)pps->cross_component_prediction_enabled;
    x->chromaQpAdjustmentEnableFlag = (unsigned)pps->chroma_qp_offset_list_enabled;
    x->diffCuChromaQpAdjustmentDepth = (unsigned)pps->diff_cu_chroma_qp_offset_depth;
    x->chromaQpAdjustmentTableSize = (unsigned)pps->chroma_qp_offset_list_len;
    x->log2SaoOffsetScaleLuma = (unsigned)pps->log2_sao_offset_scale_luma; x->log2SaoOffsetScaleChroma = (unsigned)pps->log2_sao_offset_scale_chroma;
    for (int i = 0; i < 6; i++) { x->cb_qp_adjustment[i] = (char)pps->cb_qp_offset_list[i]; x->cr_qp_adjustment[i] = (char)pps->cr_qp_offset_list[i]; }
    x->HevcFltAboveOffset = (unsigned)(d->flt_above >> 8); x->HevcSaoAboveOffset = (unsigned)(d->flt_sao_above >> 8);
    ps->v3.HevcSliceEdgeOffset = (unsigned)(d->flt_edge >> 8);
    // tile sizes in CTBs, per tile in raster order: width, height (2 bytes each, GIP_ASIC_TILE_SIZE)
    int cols[21], rows[23]; hevc_tiles(sps, pps, cols, rows);
    uint16_t *ts = d->tiles.cpu; memset(ts, 0, GIP_ASIC_TILE_SIZE);
    for (int y = 0, k = 0; y < pps->num_tile_rows; y++) for (int xx = 0; xx < pps->num_tile_columns; xx++, k++) { ts[2 * k] = (uint16_t)cols[xx]; ts[2 * k + 1] = (uint16_t)rows[y]; }
    write_scaling(d->scaling.cpu, pps->scaling_list_data_present ? &pps->scaling : &sps->scaling);
    (void)ctb;

    Chan *c = &d->chan;
    chan_rewind(c);
    chan_mthd(c, 0, NVC9B0_SET_APPLICATION_ID, 1, (NvU32)NVC9B0_SET_APPLICATION_ID_ID_HEVC);
    chan_mthd(c, 0, NVC9B0_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _CODEC_TYPE, NVDEC_CODEC_HEVC) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _GPTIMER_ON, 1));
    chan_mthd(c, 0, NVC9B0_SET_DRV_PIC_SETUP_OFFSET, 1, hi(d->setup.gpu));
    chan_mthd(c, 0, NVC9B0_SET_IN_BUF_BASE_OFFSET, 1, hi(d->bs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_PICTURE_INDEX, 1, (NvU32)surf);
    chan_mthd(c, 0, NVC9B0_SET_COLOC_DATA_OFFSET, 1, hi(d->coloc.gpu));
    chan_mthd(c, 0, NVC9B0_SET_NVDEC_STATUS_OFFSET, 1, hi(d->status.gpu));
    // not on Turing (0xc4b0): its class has no such method and NVIDIA's driver sends none there (VP9: given one, that engine
    // misused the buffer; docs/traces/vp9dec-gtx1650-linux); HEVC never touched it on Turing anyway
    if (!d->turing) chan_mthd(c, 0, NVC9B0_SET_INTRA_TOP_BUF_OFFSET, 1, hi(d->intra.gpu));
    chan_mthd(c, 0, NVC9B0_HEVC_SET_SCALING_LIST_OFFSET, 1, hi(d->scaling.gpu));
    chan_mthd(c, 0, NVC9B0_HEVC_SET_TILE_SIZES_OFFSET, 1, hi(d->tiles.gpu));
    chan_mthd(c, 0, NVC9B0_HEVC_SET_FILTER_BUFFER_OFFSET, 1, hi(d->filter.gpu));
    chan_mthd(c, 0, NVC9B0_HEVC_SET_SAO_BUFFER_OFFSET, 1, hi(d->filter.gpu + d->flt_sao));
    chan_mthd(c, 0, NVC9B0_HEVC_SET_SLICE_INFO_BUFFER_OFFSET, 1, hi(d->slinfo.gpu));
    for (int i = 0; i < HEVC_SURFACES; i++) {
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_LUMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->luma[i].gpu));
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_CHROMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->chroma[i].gpu));
    }
    chan_mthd(c, 0, NVC9B0_EXECUTE, 1, 0u);
    const NvU32 val = ++d->semval; volatile NvU32 *sv = d->sem.cpu;
    chan_mthd(c, 0, NVC9B0_SEMAPHORE_A, 3, (NvU32)(d->sem.gpu >> 32), (NvU32)d->sem.gpu, val);
    chan_mthd(c, 0, NVC9B0_SEMAPHORE_D, 1, DRF_DEF(C9B0, _SEMAPHORE_D, _STRUCTURE_SIZE, _ONE) |
              DRF_DEF(C9B0, _SEMAPHORE_D, _OPERATION, _RELEASE) | DRF_DEF(C9B0, _SEMAPHORE_D, _PAYLOAD_SIZE, _32BIT));
    const double t0 = now_ms();
    memset(d->status.cpu, 0, sizeof(nvdec_status_s));
    chan_submit(c);
    while (sv[0] != val && now_ms() - t0 < 1000) { }
    d->engine_ms = now_ms() - t0;
    d->auN = 0;
    if (sv[0] != val) { fprintf(stderr, "hevcdec: no completion (error notifier 0x%x)\n", chan_error(c)); return -1; }
    const nvdec_status_s *st = d->status.cpu;
    d->err_status = st->error_status; d->ok_blocks = st->hevc.frame_status_intra_cnt + st->hevc.frame_status_inter_cnt;
    if (d->verbose)
        printf("  hevcdec: surf %2d nal %2d %c POC %4d slices %d refs %d (%d/%d/%d) -> status 0x%x, intra %u inter %u, err ctb 0x%x, %.2f ms\n",
               surf, t, "BPI"[sl->slice_type % 3], poc, nAu, nref, r.n_before, r.n_after, r.n_lt, d->err_status,
               st->hevc.frame_status_intra_cnt, st->hevc.frame_status_inter_cnt, st->hevc.error_ctb_pos, d->engine_ms);
    d->poc[surf] = poc; d->ref[surf] = 1;                   // the current picture is a short-term reference (8.3.2)
    d->busy[surf] = true; d->cur_surf = surf; d->cur_poc = poc;
    d->cur_w = sps->width - sps->conf_win_left - sps->conf_win_right; d->cur_h = sps->height - sps->conf_win_top - sps->conf_win_bottom;
    d->cur_crop[0] = sps->conf_win_left; d->cur_crop[1] = sps->conf_win_right; d->cur_crop[2] = sps->conf_win_top; d->cur_crop[3] = sps->conf_win_bottom;
    d->cur_output = sl->pic_output_flag; d->cur_irap_nro = irap && nro;
    // NoOutputOfPriorPicsFlag (C.5.2.2): 1 for a CRA that starts decoding after an end of sequence, else the slice's flag
    d->cur_no_output_prior = irap && nro && (t == HEVC_NAL_CRA ? cra_after_eos : sl->no_output_of_prior_pics);
    d->max_reorder = sps->max_num_reorder[sps->max_sub_layers_minus1]; d->max_dec_pic = sps->max_dec_pic_buffering[sps->max_sub_layers_minus1];
    { const int hl = sps->max_sub_layers_minus1, lp = sps->max_latency_increase_plus1[hl]; d->max_latency = lp ? sps->max_num_reorder[hl] + lp - 1 : 0; }
    return 1;
}

int hevcdec_flush(HevcDec *d) { return decode_pending(d); }

int hevcdec_nal(HevcDec *d, const uint8_t *nal, size_t len)
{
    if (len < 2) return 0;
    const int t = (nal[0] >> 1) & 63, layer = (nal[0] & 1) << 5 | nal[1] >> 3;
    if (layer) return 0;                                       // base layer only
    if (t <= HEVC_NAL_RSV_IRAP_23) {
        if (t > HEVC_NAL_CRA || (t >= 10 && t <= 15)) return 0;   // reserved types
        HevcSlice s;
        if (hevc_parse_slice_header(&d->params, nal, len, &s)) { fprintf(stderr, "hevcdec: slice header parse failed\n"); return -1; }
        int r = 0;
        if (d->auSlices && s.first_slice_segment_in_pic) r = decode_pending(d);
        if (r < 0) return r;
        if (!d->auSlices) {
            if (!s.first_slice_segment_in_pic) return r;      // the start of the picture was lost
            d->auFirst = s;
        }
        if (d->auN + len + 3 > d->auCap) { fprintf(stderr, "hevcdec: access unit too large\n"); return -1; }
        d->auSlices++;
        d->au[d->auN++] = 0; d->au[d->auN++] = 0; d->au[d->auN++] = 1; memcpy(d->au + d->auN, nal, len); d->auN += len;
        return r;
    }
    int r = 0;
    if ((t >= HEVC_NAL_VPS && t <= HEVC_NAL_EOB) || t == HEVC_NAL_SEI_PREFIX || (t >= 41 && t <= 44) || (t >= 48 && t <= 55)) r = decode_pending(d);
    if (t == HEVC_NAL_SPS) hevc_parse_sps(&d->params, nal, len);
    else if (t == HEVC_NAL_PPS) hevc_parse_pps(&d->params, nal, len);
    else if (t == HEVC_NAL_EOS || t == HEVC_NAL_EOB) { d->after_eos = true; d->pocst.prev_poc_tid0 = 0; }
    return r;
}
