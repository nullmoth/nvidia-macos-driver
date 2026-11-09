// dec.c: see dec.h. Section numbers refer to ITU-T H.264.
#include "dec.h"
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

int h264dec_open(H264Dec *d, Rm *rm, unsigned maxW, unsigned maxH)
{
    memset(d, 0, sizeof *d);
    d->rm = rm; d->maxW = (maxW + 15) & ~15u; d->maxH = (maxH + 15) & ~15u; d->max_long_term_frame_idx = -1; d->prev_ref_frame_num = -1; d->last_ref_surf = -1;
    d->pitch = ((size_t)d->maxW + 63) & ~(size_t)63;
    NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = 0 };
    if (chan_create(rm, &d->chan, NV2080_ENGINE_TYPE_NVDEC0, rm_class(rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "dec: channel failed\n"); return -1; }
    const size_t mbs = (size_t)(d->maxW / 16) * (d->maxH / 16);
    d->auCap = (size_t)d->maxW * d->maxH * 2 > (4u << 20) ? (size_t)d->maxW * d->maxH * 2 : (4u << 20);
    if (!(d->au = malloc(d->auCap))) return -1;
    const size_t luma = nvenc_in_plane_size(d->pitch, d->maxH) + 65536, chroma = nvenc_in_plane_size(d->pitch, d->maxH / 2) + 65536;
    struct { RmMem *m; size_t size; } b[] = {
        { &d->setup, 0x10000 }, { &d->bs, d->auCap + 0x10000 }, { &d->offs, 0x2000 }, { &d->status, 0x1000 },
        { &d->coloc, mbs * 256 * DEC_SURFACES }, { &d->hist, 8u << 20 }, { &d->mbhist, 8u << 20 }, { &d->sem, 0x1000 } };
    for (size_t i = 0; i < sizeof b / sizeof b[0]; i++)
        if (rm_mem_alloc(rm, b[i].m, (b[i].size + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "dec: alloc failed\n"); return -1; }
    for (int i = 0; i < DEC_SURFACES; i++)
        if (rm_mem_alloc(rm, &d->luma[i], (luma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true) ||
            rm_mem_alloc(rm, &d->chroma[i], (chroma + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "dec: surface alloc failed\n"); return -1; }
    memset(d->coloc.cpu, 0, d->coloc.size); memset(d->hist.cpu, 0, d->hist.size); memset(d->mbhist.cpu, 0, d->mbhist.size);
    chan_mthd(&d->chan, 0, NVC56F_SET_OBJECT, 1, rm_class(rm, RM_CLS_NVDEC));
    d->auN = 0; d->auSlices = 0;
    return 0;
}

void h264dec_close(H264Dec *d)
{
    chan_destroy(&d->chan);
    RmMem *m[] = { &d->setup, &d->bs, &d->offs, &d->status, &d->coloc, &d->hist, &d->mbhist, &d->sem };
    for (size_t i = 0; i < sizeof m / sizeof m[0]; i++) rm_mem_free(d->rm, m[i]);
    for (int i = 0; i < DEC_SURFACES; i++) { rm_mem_free(d->rm, &d->luma[i]); rm_mem_free(d->rm, &d->chroma[i]); }
    free(d->au); d->au = NULL;
}

void h264dec_release(H264Dec *d, int surf) { if (surf >= 0 && surf < DEC_SURFACES) d->busy[surf] = false; }

void h264dec_read_nv12(H264Dec *d, int surf, uint8_t *y, size_t ystride, uint8_t *uv, size_t uvstride, unsigned w, unsigned h)
{
    const unsigned x0 = (unsigned)d->x0[surf], y0 = (unsigned)d->y0[surf];
    if (!x0 && !y0) {
        nvenc_in_untile_plane(y, ystride, d->luma[surf].cpu, w, h, d->pitch);
        nvenc_in_untile_plane(uv, uvstride, d->chroma[surf].cpu, w, h / 2, d->pitch);
        return;
    }
    // cropped on the left / top (rare): sample by sample
    for (unsigned r = 0; r < h; r++) for (unsigned x = 0; x < w; x++)
        y[r * ystride + x] = ((const uint8_t *)d->luma[surf].cpu)[nvenc_in_offset(x0 + x, y0 + r, d->pitch)];
    for (unsigned r = 0; r < h / 2; r++) for (unsigned x = 0; x < w; x++)
        uv[r * uvstride + x] = ((const uint8_t *)d->chroma[surf].cpu)[nvenc_in_offset((x0 & ~1u) + x, y0 / 2 + r, d->pitch)];
}

void h264dec_read_i420(H264Dec *d, int surf, uint8_t *dst, unsigned w, unsigned h)
{
    const uint8_t *L = d->luma[surf].cpu, *C = d->chroma[surf].cpu;
    uint8_t *u = dst + (size_t)w * h, *v = u + (size_t)w * h / 4;
    const unsigned x0 = (unsigned)d->x0[surf], y0 = (unsigned)d->y0[surf];
    for (unsigned y = 0; y < h; y++) for (unsigned x = 0; x < w; x++) dst[(size_t)y * w + x] = L[nvenc_in_offset(x0 + x, y0 + y, d->pitch)];
    for (unsigned y = 0; y < h / 2; y++) for (unsigned x = 0; x < w; x++) {
        uint8_t c = C[nvenc_in_offset((x0 & ~1u) + x, y0 / 2 + y, d->pitch)];
        if (x & 1) v[(size_t)y * (w / 2) + x / 2] = c; else u[(size_t)y * (w / 2) + x / 2] = c;
    }
}

// ---- decoded reference picture marking (8.2.5), per field ----
static int max_frame_num(const H264Sps *s) { return 1 << (s->log2_max_frame_num_minus4 + 4); }
static int fnwrap(const DecRef *r, int cur_fn, int maxfn) { return r->frame_num > cur_fn ? r->frame_num - maxfn : r->frame_num; }
static void upd(DecRef *r) { r->ref = (r->mark[0] == 2 || r->mark[1] == 2) ? 2 : (r->mark[0] || r->mark[1]) ? 1 : 0; }
static void unmark_long(DecRef *r) { for (int p = 0; p < 2; p++) if (r->mark[p] == 2) r->mark[p] = 0; upd(r); }
// A reference keeps its dpb[] slot for as long as it is a reference: the co-located data of a picture names its
// references by slot, so moving entries broke temporal direct prediction (T8.04). Empty slots have ref 0; ndpb is the
// number of slots in use up to the highest one.
static void trim(H264Dec *d) { while (d->ndpb > 0 && !d->dpb[d->ndpb - 1].ref) d->ndpb--; }

// after decoding a reference picture (frame or field): MMCO / sliding window, then the picture itself
static void mark(H264Dec *d, const H264Sps *s, const H264Slice *sl, int surf, int top, int bot)
{
    if (!sl->nal_ref_idc) return;
    const bool field = sl->field_pic_flag; const int par = sl->bottom_field_flag;
    const int maxfn = max_frame_num(s), cur = sl->frame_num, cpn = field ? 2 * cur + 1 : cur;
    int me = -1;                                                // the frame store of the current picture's first field
    if (field && !sl->nonexist_marker) for (int i = 0; i < d->ndpb; i++)
        if (d->dpb[i].ref && !d->dpb[i].nonexist && d->dpb[i].surf == surf && d->dpb[i].frame_num == cur) me = i;
    int curMark = 1, curLt = 0, fn = cur;
    if (sl->idr) {
        if (me < 0) { for (int i = 0; i < d->ndpb; i++) { d->dpb[i].mark[0] = d->dpb[i].mark[1] = 0; upd(&d->dpb[i]); } }
        if (sl->long_term_reference_flag) { curMark = 2; curLt = 0; d->max_long_term_frame_idx = 0; }
        else if (me < 0) d->max_long_term_frame_idx = -1;
    } else if (sl->adaptive_ref_pic_marking_mode_flag) {
        for (int k = 0; k < sl->n_mmco; k++) {
            const H264Mmco *m = &sl->mmco[k];
            if (m->op == 1 || m->op == 3) {                     // picNumX = CurrPicNum - (difference_of_pic_nums_minus1 + 1)
                const int x = cpn - (m->difference_of_pic_nums_minus1 + 1);
                for (int i = 0; i < d->ndpb; i++) {
                    DecRef *r = &d->dpb[i]; const int w = fnwrap(r, cur, maxfn);
                    for (int p = 0; p < 2; p++) {
                        // frames: frames / pairs with both fields short-term, PicNum = FrameNumWrap; fields: each field,
                        // PicNum = 2 FrameNumWrap + 1 (same parity) or 2 FrameNumWrap (opposite parity)
                        const bool hit = field ? (r->mark[p] == 1 && (p == par ? 2 * w + 1 : 2 * w) == x)
                                               : (p == 0 && r->mark[0] == 1 && r->mark[1] == 1 && w == x);
                        if (!hit) continue;
                        if (m->op == 1) { if (field) r->mark[p] = 0; else r->mark[0] = r->mark[1] = 0; }
                        else {
                            for (int j = 0; j < d->ndpb; j++) if (j != i && d->dpb[j].ref == 2 && d->dpb[j].long_term_frame_idx == m->long_term_frame_idx) unmark_long(&d->dpb[j]);
                            if (r->ref == 2 && r->long_term_frame_idx != m->long_term_frame_idx) unmark_long(r);
                            if (field) r->mark[p] = 2; else r->mark[0] = r->mark[1] = 2;
                            r->long_term_frame_idx = m->long_term_frame_idx;
                        }
                        upd(r);
                    }
                }
            } else if (m->op == 2) {                            // LongTermPicNum = LongTermFrameIdx (frames), 2 x + 1 / 2 x (fields)
                for (int i = 0; i < d->ndpb; i++) {
                    DecRef *r = &d->dpb[i]; const int lt = r->long_term_frame_idx;
                    if (!field) { if (r->mark[0] == 2 && r->mark[1] == 2 && lt == m->long_term_pic_num) { r->mark[0] = r->mark[1] = 0; upd(r); } }
                    else for (int p = 0; p < 2; p++) if (r->mark[p] == 2 && (p == par ? 2 * lt + 1 : 2 * lt) == m->long_term_pic_num) { r->mark[p] = 0; upd(r); }
                }
            } else if (m->op == 4) {
                d->max_long_term_frame_idx = m->max_long_term_frame_idx_plus1 - 1;
                for (int i = 0; i < d->ndpb; i++) if (d->dpb[i].ref == 2 && d->dpb[i].long_term_frame_idx > d->max_long_term_frame_idx) unmark_long(&d->dpb[i]);
            } else if (m->op == 5) {
                for (int i = 0; i < d->ndpb; i++) { d->dpb[i].mark[0] = d->dpb[i].mark[1] = 0; upd(&d->dpb[i]); }
                d->max_long_term_frame_idx = -1; me = -1;
                fn = 0;                                         // the current picture becomes frame_num 0, POC relative to itself
                const int mn = field ? (par ? bot : top) : (top < bot ? top : bot); top -= mn; bot -= mn;
            } else if (m->op == 6) {
                for (int i = 0; i < d->ndpb; i++) if (i != me && d->dpb[i].ref == 2 && d->dpb[i].long_term_frame_idx == m->long_term_frame_idx) unmark_long(&d->dpb[i]);
                curMark = 2; curLt = m->long_term_frame_idx;
            }
        }
    } else if (me < 0) {                                        // sliding window (8.2.5.3), not for the second field of a pair
        int ns = 0, nl = 0;
        for (int i = 0; i < d->ndpb; i++) { const DecRef *r = &d->dpb[i]; if (r->mark[0] == 1 || r->mark[1] == 1) ns++; else if (r->ref) nl++; }
        const int maxref = s->max_num_ref_frames > 0 ? s->max_num_ref_frames : 1;
        if (ns + nl >= maxref && ns > 0) {
            int best = -1, bw = 0;
            for (int i = 0; i < d->ndpb; i++) if (d->dpb[i].mark[0] == 1 || d->dpb[i].mark[1] == 1) {
                const int w = fnwrap(&d->dpb[i], cur, maxfn);
                if (best < 0 || w < bw) { best = i; bw = w; }
            }
            DecRef *r = &d->dpb[best]; for (int p = 0; p < 2; p++) if (r->mark[p] == 1) r->mark[p] = 0; upd(r);
        }
    }
    if (me >= 0) {                                              // the second field joins its first field's frame store
        DecRef *r = &d->dpb[me]; r->mark[par] = curMark; if (curMark == 2) r->long_term_frame_idx = curLt;
        if (par) r->bot_poc = bot; else r->top_poc = top;
        upd(r);
    } else {
        DecRef n = { .surf = surf, .frame_num = fn, .long_term_frame_idx = curLt, .top_poc = top, .bot_poc = bot, .fields = field, .nonexist = sl->nonexist_marker };
        if (field) n.mark[par] = curMark; else n.mark[0] = n.mark[1] = curMark;
        upd(&n);
        int slot = 0; while (slot < d->ndpb && d->dpb[slot].ref) slot++;
        if (slot < 16) { d->dpb[slot] = n; if (slot == d->ndpb) d->ndpb++; }
    }
    trim(d);
}

static void push_out(H264Dec *d, int surf, int top, int bot, int w, int h)
{
    if (d->nout < 4) d->out[d->nout++] = (DecOut){ surf, top, bot, w, h };
}
bool h264dec_pop(H264Dec *d)
{
    if (!d->nout) return false;
    const DecOut o = d->out[0];
    memmove(d->out, d->out + 1, sizeof d->out[0] * (size_t)--d->nout);
    d->cur_surf = o.surf; d->cur_top = o.top; d->cur_bot = o.bot; d->cur_w = o.w; d->cur_h = o.h;
    return true;
}
static int complete_field(H264Dec *d)   // a waiting first field without a second: a non-paired field, output as it is
{
    if (!d->fld.valid) return 0;
    d->fld.valid = false;
    push_out(d, d->fld.surf, d->fld.top, d->fld.bot, d->fld.w, d->fld.h);
    return 1;
}

// ---- one access unit ----
static int decode_pending(H264Dec *d)
{
    if (!d->auSlices) return 0;
    const H264Slice *sl = &d->auFirst;
    const H264Pps *pps = &d->params.pps[sl->pps_id]; const H264Sps *sps = &d->params.sps[pps->sps_id];
    const unsigned wmb = (unsigned)sps->pic_width_in_mbs_minus1 + 1, hmb = (unsigned)(sps->pic_height_in_map_units_minus1 + 1) * (2u - (unsigned)sps->frame_mbs_only_flag);
    if (wmb * 16 > d->maxW || hmb * 16 > d->maxH) { fprintf(stderr, "dec: %ux%u is larger than the session (%ux%u)\n", wmb * 16, hmb * 16, d->maxW, d->maxH); return -1; }
    const bool field = sl->field_pic_flag; const int par = sl->bottom_field_flag;
    // a field picture is the second field of the waiting first field when it has the opposite parity and the same
    // frame_num (7.4.1.2.4 / 8.2.1); anything else completes the waiting field as a non-paired one
    const bool second = field && d->fld.valid && d->fld.par != par && d->fld.frame_num == sl->frame_num && !(sl->idr && !d->fld.ref);
    int done = second ? 0 : complete_field(d);
    // gaps in frame_num (8.2.5.2): each missing frame_num becomes a "non-existing" short-term reference frame, marked
    // by the sliding window, so that later sliding windows, picture numbers (MMCO) and the default reference lists match;
    // it is never used for prediction and points at the newest real reference's surface
    if (!sl->idr && !second && d->prev_ref_frame_num >= 0 && d->last_ref_surf >= 0) {
        const int maxfn = max_frame_num(sps);
        int fn = (d->prev_ref_frame_num + 1) % maxfn;
        if (sl->frame_num != d->prev_ref_frame_num && sl->frame_num != fn) {
            if (d->verbose) printf("  dec: frame_num gap %d -> %d%s\n", d->prev_ref_frame_num, sl->frame_num, sps->gaps_in_frame_num_value_allowed_flag ? "" : " (not allowed by the SPS)");
            for (int k = 0; fn != sl->frame_num && k < maxfn; fn = (fn + 1) % maxfn, k++) {
                H264Slice ne = { 0 }; ne.frame_num = fn; ne.nal_ref_idc = 1; ne.nonexist_marker = 1;
                mark(d, sps, &ne, d->last_ref_surf, d->last_top, d->last_bot);
            }
        }
    }
    int top, bot; h264_poc(&d->poc, sps, sl, &top, &bot);
    // a free surface: not held by the caller, not a reference
    int surf = second ? d->fld.surf : -1;
    for (int i = 0; i < DEC_SURFACES && surf < 0; i++) {
        bool used = d->busy[i];
        for (int k = 0; k < d->ndpb; k++) if (d->dpb[k].ref && d->dpb[k].surf == i) used = true;
        if (!used) surf = i;
    }
    if (surf < 0) { fprintf(stderr, "dec: no free surface (the caller holds too many)\n"); return -1; }

    // end of picture: eos[] + explicitEOSPresentFlag (T8.03)
    static const uint8_t eos[16] = { 0, 0, 1, 0x0b, 0, 0, 1, 0x0b };
    memcpy(d->bs.cpu, d->au, d->auN);
    memcpy(d->offs.cpu, d->auOff, sizeof(NvU32) * (size_t)d->auSlices);
    nvdec_h264_pic_s *ps = d->setup.cpu; memset(ps, 0, sizeof *ps);
    memcpy(ps->eos, eos, 16); ps->explicitEOSPresentFlag = 1;
    ps->stream_len = (unsigned)d->auN; ps->slice_count = (unsigned)d->auSlices;
    ps->mbhist_buffer_size = (unsigned)d->mbhist.size;
    ps->log2_max_pic_order_cnt_lsb_minus4 = sps->log2_max_pic_order_cnt_lsb_minus4;
    ps->delta_pic_order_always_zero_flag = sps->delta_pic_order_always_zero_flag;
    ps->frame_mbs_only_flag = sps->frame_mbs_only_flag;
    ps->PicWidthInMbs = (int)wmb; ps->FrameHeightInMbs = (int)hmb;
    ps->tileFormat = 1; ps->gob_height = 0;
    ps->entropy_coding_mode_flag = pps->entropy_coding_mode_flag;
    ps->pic_order_present_flag = pps->bottom_field_pic_order_in_frame_present_flag;
    ps->num_ref_idx_l0_active_minus1 = pps->num_ref_idx_l0_default_active_minus1;
    ps->num_ref_idx_l1_active_minus1 = pps->num_ref_idx_l1_default_active_minus1;
    ps->deblocking_filter_control_present_flag = pps->deblocking_filter_control_present_flag;
    ps->redundant_pic_cnt_present_flag = pps->redundant_pic_cnt_present_flag;
    ps->transform_8x8_mode_flag = pps->transform_8x8_mode_flag;
    ps->pitch_luma = ps->pitch_chroma = (unsigned)d->pitch;
    ps->HistBufferSize = (unsigned)(d->hist.size >> 8);
    ps->MbaffFrameFlag = sps->mb_adaptive_frame_field_flag && !field;
    ps->field_pic_flag = field; ps->bottom_field_flag = (unsigned)par; ps->second_field = second;
    ps->direct_8x8_inference_flag = sps->direct_8x8_inference_flag;
    ps->weighted_pred_flag = pps->weighted_pred_flag; ps->weighted_bipred_idc = pps->weighted_bipred_idc;
    ps->constrained_intra_pred_flag = pps->constrained_intra_pred_flag;
    ps->ref_pic_flag = sl->nal_ref_idc != 0;
    ps->log2_max_frame_num_minus4 = sps->log2_max_frame_num_minus4;
    ps->chroma_format_idc = sps->chroma_format_idc;
    ps->pic_order_cnt_type = sps->pic_order_cnt_type;
    ps->pic_init_qp_minus26 = pps->pic_init_qp_minus26;
    ps->chroma_qp_index_offset = pps->chroma_qp_index_offset;
    ps->second_chroma_qp_index_offset = pps->second_chroma_qp_index_offset;
    ps->CurrPicIdx = (unsigned)surf; ps->CurrColIdx = (unsigned)surf;
    ps->frame_num = (unsigned)sl->frame_num; ps->frame_surfaces = 1;
    if (second) { if (par) top = d->fld.top; else bot = d->fld.bot; }   // the pair's other field
    else if (field) { if (par) top = bot; else bot = top; }
    ps->CurrFieldOrderCnt[0] = top; ps->CurrFieldOrderCnt[1] = bot;
    int nref = 0;
    for (int i = 0; i < d->ndpb; i++) {                        // each reference in its own stable slot; empty slots stay zero
        const DecRef *r = &d->dpb[i]; nvdec_dpb_entry_s *e = &ps->dpb[i];
        if (!r->ref) continue;
        nref++;
        e->index = (unsigned)r->surf; e->col_idx = (unsigned)r->surf;
        e->state = (r->mark[0] ? 1u : 0u) | (r->mark[1] ? 2u : 0u);
        e->is_long_term = r->ref == 2; e->is_field = r->fields;
        e->top_field_marking = (unsigned)r->mark[0]; e->bottom_field_marking = (unsigned)r->mark[1];
        e->FieldOrderCnt[0] = (unsigned)r->top_poc; e->FieldOrderCnt[1] = (unsigned)r->bot_poc;
        e->FrameIdx = r->ref == 2 ? r->long_term_frame_idx : r->frame_num;
    }
    for (int i = 0; i < 6; i++) for (int k = 0; k < 16; k++) ((uint8_t *)ps->WeightScale[i])[h264_zigzag4x4[k]] = pps->scaling4x4[i][k];
    for (int i = 0; i < 2; i++) for (int k = 0; k < 64; k++) ((uint8_t *)ps->WeightScale8x8[i])[h264_zigzag8x8[k]] = pps->scaling8x8[i][k];
    ps->qpprime_y_zero_transform_bypass_flag = sps->qpprime_y_zero_transform_bypass_flag;

    Chan *c = &d->chan;
    chan_rewind(c);
    chan_mthd(c, 0, NVC9B0_SET_APPLICATION_ID, 1, (NvU32)NVC9B0_SET_APPLICATION_ID_ID_H264);
    chan_mthd(c, 0, NVC9B0_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _CODEC_TYPE, NVDEC_CODEC_H264) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _GPTIMER_ON, 1));
    chan_mthd(c, 0, NVC9B0_SET_DRV_PIC_SETUP_OFFSET, 1, hi(d->setup.gpu));
    chan_mthd(c, 0, NVC9B0_SET_IN_BUF_BASE_OFFSET, 1, hi(d->bs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_PICTURE_INDEX, 1, (NvU32)surf);
    chan_mthd(c, 0, NVC9B0_SET_SLICE_OFFSETS_BUF_OFFSET, 1, hi(d->offs.gpu));
    chan_mthd(c, 0, NVC9B0_SET_COLOC_DATA_OFFSET, 1, hi(d->coloc.gpu));
    chan_mthd(c, 0, NVC9B0_SET_HISTORY_OFFSET, 1, hi(d->hist.gpu));
    chan_mthd(c, 0, NVC9B0_SET_NVDEC_STATUS_OFFSET, 1, hi(d->status.gpu));
    chan_mthd(c, 0, NVC9B0_H264_SET_MBHIST_BUF_OFFSET, 1, hi(d->mbhist.gpu));
    for (int i = 0; i < DEC_SURFACES; i++) {
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_LUMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->luma[i].gpu));
        chan_mthd(c, 0, NVC9B0_SET_PICTURE_CHROMA_OFFSET0 + 4 * (NvU32)i, 1, hi(d->chroma[i].gpu));
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
    if (sv[0] != val) { fprintf(stderr, "dec: no completion (error notifier 0x%x)\n", chan_error(c)); return -1; }
    const nvdec_status_s *st = d->status.cpu;
    d->mbs_ok = st->mbs_correctly_decoded; d->mbs_err = st->mbs_in_error; d->err_status = st->error_status;
    if (d->verbose)
        printf("  dec: surf %2d %c%s frame_num %3d POC %4d/%4d refs %d -> %u MBs ok, %u err, status 0x%x, %.2f ms\n", surf, "PBIps"[sl->slice_type],
               field ? (second ? (par ? " bot2" : " top2") : (par ? " bot1" : " top1")) : "", sl->frame_num, top, bot, nref, d->mbs_ok, d->mbs_err, d->err_status, d->engine_ms);
    mark(d, sps, sl, surf, top, bot);
    if (sl->nal_ref_idc) { d->prev_ref_frame_num = sl->has_mmco5 ? 0 : sl->frame_num; d->last_ref_surf = surf; d->last_top = top; d->last_bot = bot; }
    d->busy[surf] = true; d->x0[surf] = sps->crop_x0; d->y0[surf] = sps->crop_y0;
    d->auN = 0; d->auSlices = 0;
    if (field && !second) {                                     // wait for the second field
        d->fld.valid = true; d->fld.surf = surf; d->fld.par = par; d->fld.frame_num = sl->frame_num; d->fld.ref = sl->nal_ref_idc != 0;
        d->fld.top = top; d->fld.bot = bot; d->fld.w = sps->width; d->fld.h = sps->height;
        return done;
    }
    d->fld.valid = false;
    push_out(d, surf, top, bot, sps->width, sps->height);
    return done + 1;
}

int h264dec_flush(H264Dec *d)
{
    const int r = decode_pending(d);
    return r < 0 ? r : r + complete_field(d);
}

int h264dec_nal(H264Dec *d, const uint8_t *nal, size_t len)
{
    if (!len) return 0;
    const int t = nal[0] & 31;
    if (t == H264_NAL_SLICE || t == H264_NAL_IDR) {
        H264Slice s;
        const int pr = h264_parse_slice_header(&d->params, nal, len, &s);
        if (pr == -2) { fprintf(stderr, "dec: slice groups (FMO) are not supported by NVDEC\n"); return -1; }
        if (pr) { fprintf(stderr, "dec: slice header parse failed\n"); return -1; }
        if (s.slice_type == 3 || s.slice_type == 4) { fprintf(stderr, "dec: SP / SI slices (Extended profile) are not supported by NVDEC\n"); return -1; }
        int r = 0;
        if (s.redundant_pic_cnt > 0) return 0;                  // redundant slices: the primary picture is complete without them
        if (d->auSlices && h264_new_picture(&d->params, &d->auLast, &s)) r = decode_pending(d);   // a new picture starts (7.4.1.2.4)
        if (r < 0) return r;
        if (!d->auSlices) d->auFirst = s;
        d->auLast = s;
        if (d->auSlices >= 1024 || d->auN + len + 3 > d->auCap) { fprintf(stderr, "dec: access unit too large\n"); return -1; }
        d->auOff[d->auSlices++] = (NvU32)d->auN;
        d->au[d->auN++] = 0; d->au[d->auN++] = 0; d->au[d->auN++] = 1; memcpy(d->au + d->auN, nal, len); d->auN += len;
        return r;
    }
    // any other NAL unit before a picture's slices ends the previous picture
    int r = (t == H264_NAL_SPS || t == H264_NAL_PPS || t == H264_NAL_AUD || t == H264_NAL_SEI || t == H264_NAL_EOSEQ) ? decode_pending(d) : 0;
    if (t == H264_NAL_SPS) h264_parse_sps(&d->params, nal, len);
    else if (t == H264_NAL_PPS) h264_parse_pps(&d->params, nal, len);
    return r;
}
