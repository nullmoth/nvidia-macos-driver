// h264parse.c: see h264parse.h. Section numbers refer to ITU-T H.264 (08/2021).
#include "h264parse.h"
#include <string.h>

const uint8_t h264_zigzag4x4[16] = { 0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15 };
const uint8_t h264_zigzag8x8[64] = {
     0,  1,  8, 16,  9,  2,  3, 10, 17, 24, 32, 25, 18, 11,  4,  5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63 };
// Table 7-3 / 7-4, in zig-zag order
static const uint8_t def4x4_intra[16] = { 6, 13, 13, 20, 20, 20, 28, 28, 28, 28, 32, 32, 32, 37, 37, 42 };
static const uint8_t def4x4_inter[16] = { 10, 14, 14, 20, 20, 20, 24, 24, 24, 24, 27, 27, 27, 30, 30, 34 };
static const uint8_t def8x8_intra[64] = {
     6, 10, 10, 13, 11, 13, 16, 16, 16, 16, 18, 18, 18, 18, 18, 23, 23, 23, 23, 23, 23, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27,
    27, 27, 27, 27, 29, 29, 29, 29, 29, 29, 29, 31, 31, 31, 31, 31, 31, 33, 33, 33, 33, 33, 36, 36, 36, 36, 38, 38, 38, 40, 40, 42 };
static const uint8_t def8x8_inter[64] = {
     9, 13, 13, 15, 13, 15, 17, 17, 17, 17, 19, 19, 19, 19, 19, 21, 21, 21, 21, 21, 21, 22, 22, 22, 22, 22, 22, 22, 24, 24, 24, 24,
    24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 28, 28, 28, 28, 28, 30, 30, 30, 30, 32, 32, 32, 33, 33, 35 };

// ---- NAL units ----
bool h264_next_annexb(const uint8_t *b, size_t n, size_t *pos, const uint8_t **nal, size_t *len)
{
    size_t i = *pos;
    while (i + 3 <= n && !(b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1)) i++;
    if (i + 3 > n) { *pos = n; return false; }
    size_t s = i + 3, e = s;
    while (e + 3 <= n && !(b[e] == 0 && b[e + 1] == 0 && (b[e + 2] == 1 || (b[e + 2] == 0 && e + 3 < n && b[e + 3] == 1)))) e++;
    if (e + 3 > n) e = n;
    while (e > s && b[e - 1] == 0) e--;                     // trailing_zero_8bits
    *nal = b + s; *len = e - s; *pos = e;
    return *len > 0;
}
bool h264_next_lenpfx(const uint8_t *b, size_t n, int k, size_t *pos, const uint8_t **nal, size_t *len)
{
    size_t i = *pos;
    if (k < 1 || k > 4 || i + (size_t)k > n) return false;
    size_t l = 0; for (int j = 0; j < k; j++) l = l << 8 | b[i + j];
    if (l == 0 || i + k + l > n) return false;
    *nal = b + i + k; *len = l; *pos = i + k + l;
    return true;
}

// ---- RBSP bit reader ----
typedef struct { uint8_t buf[4096]; size_t n, bit; bool err; } Br;
static void br_init(Br *r, const uint8_t *nal, size_t len)   // skips the 1-byte NAL header, removes emulation prevention
{
    r->n = 0; r->bit = 0; r->err = false;
    int zeros = 0;
    for (size_t i = 1; i < len && r->n < sizeof r->buf; i++) {
        if (zeros >= 2 && nal[i] == 3) { zeros = 0; continue; }
        zeros = nal[i] == 0 ? zeros + 1 : 0;
        r->buf[r->n++] = nal[i];
    }
}
static unsigned u1(Br *r) { if (r->bit >= r->n * 8) { r->err = true; return 0; } unsigned v = r->buf[r->bit >> 3] >> (7 - (r->bit & 7)) & 1; r->bit++; return v; }
static unsigned u(Br *r, int n) { unsigned v = 0; while (n-- > 0) v = v << 1 | u1(r); return v; }
static unsigned ue(Br *r) { int z = 0; while (!u1(r)) { if (++z > 31 || r->err) { r->err = true; return 0; } } return ((1u << z) - 1) + u(r, z); }
static int se(Br *r) { unsigned k = ue(r); return (k & 1) ? (int)((k + 1) >> 1) : -(int)(k >> 1); }
static bool more_rbsp_data(Br *r)
{
    if (r->bit >= r->n * 8) return false;
    size_t last = r->n; while (last > 0 && r->buf[last - 1] == 0) last--;   // the rbsp_stop_one_bit is the last 1 bit
    if (last == 0) return false;
    unsigned char c = r->buf[last - 1]; int t = 0; while (!(c >> t & 1)) t++;
    size_t stop = (last - 1) * 8 + (7 - t);
    return r->bit < stop;
}

// ---- scaling lists (7.3.2.1.1.1, Table 7-2) ----
static void scaling_list(Br *r, uint8_t *list, int size, bool *use_default)
{
    int last = 8, next = 8; *use_default = false;
    for (int j = 0; j < size; j++) {
        if (next) { int d = se(r); next = (last + d + 256) % 256; *use_default = (j == 0 && next == 0); }
        list[j] = (uint8_t)(next == 0 ? last : next);
        last = list[j];
    }
}
// fall-back rule A (SPS) when fb4/fb8 are NULL; rule B (PPS) takes the sequence-level lists for 0, 3, 6 and 7
static void read_matrix(Br *r, int n_lists, uint8_t s4[6][16], uint8_t s8[6][64], const uint8_t (*fb4)[16], const uint8_t (*fb8)[64])
{
    for (int i = 0; i < n_lists; i++) {
        bool present = u1(r), dflt = false;
        if (i < 6) {
            if (present) { scaling_list(r, s4[i], 16, &dflt); if (dflt) memcpy(s4[i], i < 3 ? def4x4_intra : def4x4_inter, 16); }
            else if (i == 0 || i == 3) memcpy(s4[i], fb4 ? fb4[i] : (i == 0 ? def4x4_intra : def4x4_inter), 16);
            else memcpy(s4[i], s4[i - 1], 16);
        } else {
            int k = i - 6;
            if (present) { scaling_list(r, s8[k], 64, &dflt); if (dflt) memcpy(s8[k], (k & 1) ? def8x8_inter : def8x8_intra, 64); }
            else if (k < 2) memcpy(s8[k], fb8 ? fb8[k] : (k ? def8x8_inter : def8x8_intra), 64);
            else memcpy(s8[k], s8[k - 2], 64);
        }
    }
}
static void flat(uint8_t s4[6][16], uint8_t s8[6][64]) { memset(s4, 16, 6 * 16); memset(s8, 16, 6 * 64); }

// ---- VUI (E.1.1) ----
static void hrd(Br *r)
{
    unsigned cnt = ue(r); u(r, 4); u(r, 4);
    for (unsigned i = 0; i <= cnt && i < 32; i++) { ue(r); ue(r); u1(r); }
    u(r, 5); u(r, 5); u(r, 5); u(r, 5);
}
static void vui(Br *r, H264Sps *s)
{
    s->vui_present = 1; s->colour_primaries = s->transfer_characteristics = s->matrix_coefficients = 2;
    if (u1(r)) { if (u(r, 8) == 255) { u(r, 16); u(r, 16); } }          // aspect_ratio_info
    if (u1(r)) u1(r);                                                  // overscan
    if (u1(r)) {                                                       // video_signal_type
        u(r, 3); s->video_full_range_flag = (int)u1(r);
        if (u1(r)) { s->colour_primaries = (int)u(r, 8); s->transfer_characteristics = (int)u(r, 8); s->matrix_coefficients = (int)u(r, 8); }
    }
    if (u1(r)) { ue(r); ue(r); }                                       // chroma_loc_info
    if (u1(r)) { u(r, 32); u(r, 32); u1(r); }                          // timing_info
    int nal = (int)u1(r); if (nal) hrd(r);
    int vcl = (int)u1(r); if (vcl) hrd(r);
    if (nal || vcl) u1(r);                                             // low_delay_hrd_flag
    u1(r);                                                             // pic_struct_present_flag
    s->bitstream_restriction_flag = (int)u1(r);
    if (s->bitstream_restriction_flag) {
        u1(r); ue(r); ue(r); ue(r); ue(r);
        s->max_num_reorder_frames = (int)ue(r); s->max_dec_frame_buffering = (int)ue(r);
    }
}

// ---- SPS (7.3.2.1.1) ----
int h264_parse_sps(H264Params *p, const uint8_t *nal, size_t len)
{
    static Br r; br_init(&r, nal, len);
    H264Sps s; memset(&s, 0, sizeof s);
    s.profile_idc = (int)u(&r, 8); s.constraint_flags = (int)u(&r, 8); s.level_idc = (int)u(&r, 8);
    s.sps_id = (int)ue(&r);
    if (s.sps_id > 31) return -1;
    s.chroma_format_idc = 1; flat(s.scaling4x4, s.scaling8x8);
    switch (s.profile_idc) {
    case 100: case 110: case 122: case 244: case 44: case 83: case 86: case 118: case 128: case 138: case 139: case 134: case 135:
        s.chroma_format_idc = (int)ue(&r);
        if (s.chroma_format_idc == 3) s.separate_colour_plane_flag = (int)u1(&r);
        s.bit_depth_luma_minus8 = (int)ue(&r); s.bit_depth_chroma_minus8 = (int)ue(&r);
        s.qpprime_y_zero_transform_bypass_flag = (int)u1(&r);
        s.seq_scaling_matrix_present_flag = (int)u1(&r);
        if (s.seq_scaling_matrix_present_flag) read_matrix(&r, s.chroma_format_idc != 3 ? 8 : 12, s.scaling4x4, s.scaling8x8, NULL, NULL);
    }
    s.log2_max_frame_num_minus4 = (int)ue(&r);
    s.pic_order_cnt_type = (int)ue(&r);
    if (s.pic_order_cnt_type == 0) s.log2_max_pic_order_cnt_lsb_minus4 = (int)ue(&r);
    else if (s.pic_order_cnt_type == 1) {
        s.delta_pic_order_always_zero_flag = (int)u1(&r);
        s.offset_for_non_ref_pic = se(&r); s.offset_for_top_to_bottom_field = se(&r);
        s.num_ref_frames_in_pic_order_cnt_cycle = (int)ue(&r);
        if (s.num_ref_frames_in_pic_order_cnt_cycle > 255) return -1;
        for (int i = 0; i < s.num_ref_frames_in_pic_order_cnt_cycle; i++) s.offset_for_ref_frame[i] = se(&r);
    }
    s.max_num_ref_frames = (int)ue(&r); s.gaps_in_frame_num_value_allowed_flag = (int)u1(&r);
    s.pic_width_in_mbs_minus1 = (int)ue(&r); s.pic_height_in_map_units_minus1 = (int)ue(&r);
    s.frame_mbs_only_flag = (int)u1(&r);
    if (!s.frame_mbs_only_flag) s.mb_adaptive_frame_field_flag = (int)u1(&r);
    s.direct_8x8_inference_flag = (int)u1(&r);
    s.frame_cropping_flag = (int)u1(&r);
    if (s.frame_cropping_flag) { s.crop_left = (int)ue(&r); s.crop_right = (int)ue(&r); s.crop_top = (int)ue(&r); s.crop_bottom = (int)ue(&r); }
    if (u1(&r)) vui(&r, &s);
    if (r.err || s.log2_max_frame_num_minus4 > 12 || s.pic_order_cnt_type > 2 || s.log2_max_pic_order_cnt_lsb_minus4 > 12) return -1;
    int cat = s.separate_colour_plane_flag ? 0 : s.chroma_format_idc;
    int ux = cat == 0 ? 1 : (cat == 3 ? 1 : 2), uy = (cat == 0 ? 1 : (cat == 1 ? 2 : 1)) * (2 - s.frame_mbs_only_flag);
    s.width = 16 * (s.pic_width_in_mbs_minus1 + 1) - ux * (s.crop_left + s.crop_right);
    s.height = 16 * (s.pic_height_in_map_units_minus1 + 1) * (2 - s.frame_mbs_only_flag) - uy * (s.crop_top + s.crop_bottom);
    s.crop_x0 = ux * s.crop_left; s.crop_y0 = uy * s.crop_top;
    if (!s.bitstream_restriction_flag) s.max_dec_frame_buffering = s.max_num_reorder_frames = 16;
    s.valid = true;
    p->sps[s.sps_id] = s;
    return 0;
}

// ---- PPS (7.3.2.2) ----
int h264_parse_pps(H264Params *p, const uint8_t *nal, size_t len)
{
    static Br r; br_init(&r, nal, len);
    H264Pps q; memset(&q, 0, sizeof q);
    q.pps_id = (int)ue(&r); q.sps_id = (int)ue(&r);
    if (q.pps_id > 255 || q.sps_id > 31 || !p->sps[q.sps_id].valid) return -1;
    const H264Sps *s = &p->sps[q.sps_id];
    q.entropy_coding_mode_flag = (int)u1(&r); q.bottom_field_pic_order_in_frame_present_flag = (int)u1(&r);
    q.num_slice_groups_minus1 = (int)ue(&r);
    if (q.num_slice_groups_minus1) {   // FMO (Baseline / Extended): NVDEC's setup has no slice groups. The PPS is kept marked,
        p->pps[q.pps_id] = q; p->pps[q.pps_id].valid = 1;   // so its slices fail instead of decoding with an older PPS
        return -1;
    }
    q.num_ref_idx_l0_default_active_minus1 = (int)ue(&r); q.num_ref_idx_l1_default_active_minus1 = (int)ue(&r);
    q.weighted_pred_flag = (int)u1(&r); q.weighted_bipred_idc = (int)u(&r, 2);
    q.pic_init_qp_minus26 = se(&r); q.pic_init_qs_minus26 = se(&r); q.chroma_qp_index_offset = se(&r);
    q.deblocking_filter_control_present_flag = (int)u1(&r); q.constrained_intra_pred_flag = (int)u1(&r);
    q.redundant_pic_cnt_present_flag = (int)u1(&r);
    q.second_chroma_qp_index_offset = q.chroma_qp_index_offset;
    memcpy(q.scaling4x4, s->scaling4x4, sizeof q.scaling4x4); memcpy(q.scaling8x8, s->scaling8x8, sizeof q.scaling8x8);
    if (more_rbsp_data(&r)) {
        q.transform_8x8_mode_flag = (int)u1(&r);
        q.pic_scaling_matrix_present_flag = (int)u1(&r);
        if (q.pic_scaling_matrix_present_flag) {
            // rule B falls back to the sequence-level lists, or to the defaults (rule A) when the SPS has no matrix
            const bool seq = s->seq_scaling_matrix_present_flag;
            read_matrix(&r, 6 + (s->chroma_format_idc != 3 ? 2 : 6) * q.transform_8x8_mode_flag, q.scaling4x4, q.scaling8x8,
                        seq ? s->scaling4x4 : NULL, seq ? s->scaling8x8 : NULL);
        }
        q.second_chroma_qp_index_offset = se(&r);
    }
    if (r.err) return -1;
    q.valid = true;
    p->pps[q.pps_id] = q;
    return 0;
}

// ---- slice header (7.3.3), up to dec_ref_pic_marking ----
int h264_parse_slice_header(const H264Params *p, const uint8_t *nal, size_t len, H264Slice *o)
{
    static Br r; br_init(&r, nal, len);
    memset(o, 0, sizeof *o);
    o->nal_ref_idc = nal[0] >> 5 & 3; o->nal_unit_type = nal[0] & 31; o->idr = o->nal_unit_type == H264_NAL_IDR;
    o->first_mb_in_slice = (int)ue(&r); o->slice_type = (int)(ue(&r) % 5); o->pps_id = (int)ue(&r);
    if (o->pps_id > 255 || !p->pps[o->pps_id].valid) return -1;
    if (p->pps[o->pps_id].num_slice_groups_minus1) return -2;          // FMO: not decodable on NVDEC
    const H264Pps *q = &p->pps[o->pps_id]; const H264Sps *s = &p->sps[q->sps_id];
    if (!s->valid) return -1;
    if (s->separate_colour_plane_flag) o->colour_plane_id = (int)u(&r, 2);
    o->frame_num = (int)u(&r, s->log2_max_frame_num_minus4 + 4);
    if (!s->frame_mbs_only_flag) { o->field_pic_flag = (int)u1(&r); if (o->field_pic_flag) o->bottom_field_flag = (int)u1(&r); }
    if (o->idr) o->idr_pic_id = (int)ue(&r);
    if (s->pic_order_cnt_type == 0) {
        o->pic_order_cnt_lsb = (int)u(&r, s->log2_max_pic_order_cnt_lsb_minus4 + 4);
        if (q->bottom_field_pic_order_in_frame_present_flag && !o->field_pic_flag) o->delta_pic_order_cnt_bottom = se(&r);
    }
    if (s->pic_order_cnt_type == 1 && !s->delta_pic_order_always_zero_flag) {
        o->delta_pic_order_cnt[0] = se(&r);
        if (q->bottom_field_pic_order_in_frame_present_flag && !o->field_pic_flag) o->delta_pic_order_cnt[1] = se(&r);
    }
    if (q->redundant_pic_cnt_present_flag) o->redundant_pic_cnt = (int)ue(&r);
    const bool B = o->slice_type == H264_SLICE_B, P = o->slice_type == H264_SLICE_P || o->slice_type == H264_SLICE_SP;
    if (B) o->direct_spatial_mv_pred_flag = (int)u1(&r);
    o->num_ref_idx_l0_active_minus1 = q->num_ref_idx_l0_default_active_minus1;
    o->num_ref_idx_l1_active_minus1 = q->num_ref_idx_l1_default_active_minus1;
    if ((P || B) && u1(&r)) { o->num_ref_idx_l0_active_minus1 = (int)ue(&r); if (B) o->num_ref_idx_l1_active_minus1 = (int)ue(&r); }
    if (o->num_ref_idx_l0_active_minus1 > 31 || o->num_ref_idx_l1_active_minus1 > 31) return -1;
    // ref_pic_list_modification (7.3.3.1)
    for (int l = 0; l < (B ? 2 : (P ? 1 : 0)); l++)
        if (u1(&r)) for (int n = 0; n < 100; n++) {
            unsigned idc = ue(&r);
            if (idc == 3 || r.err) break;
            ue(&r);                                                    // abs_diff_pic_num_minus1 or long_term_pic_num
        }
    // pred_weight_table (7.3.3.2)
    const int cat = s->separate_colour_plane_flag ? 0 : s->chroma_format_idc;
    if ((q->weighted_pred_flag && P) || (q->weighted_bipred_idc == 1 && B)) {
        ue(&r); if (cat) ue(&r);
        for (int l = 0; l < (B ? 2 : 1); l++) {
            int n = l ? o->num_ref_idx_l1_active_minus1 : o->num_ref_idx_l0_active_minus1;
            for (int i = 0; i <= n; i++) {
                if (u1(&r)) { se(&r); se(&r); }
                if (cat && u1(&r)) { se(&r); se(&r); se(&r); se(&r); }
            }
        }
    }
    // dec_ref_pic_marking (7.3.3.3)
    if (o->nal_ref_idc) {
        if (o->idr) { o->no_output_of_prior_pics_flag = (int)u1(&r); o->long_term_reference_flag = (int)u1(&r); }
        else if ((o->adaptive_ref_pic_marking_mode_flag = (int)u1(&r)))
            for (;;) {
                H264Mmco m = { (int)ue(&r), 0, 0, 0, 0 };
                if (m.op == 0 || r.err || o->n_mmco >= 66) break;
                if (m.op == 1 || m.op == 3) m.difference_of_pic_nums_minus1 = (int)ue(&r);
                if (m.op == 2) m.long_term_pic_num = (int)ue(&r);
                if (m.op == 3 || m.op == 6) m.long_term_frame_idx = (int)ue(&r);
                if (m.op == 4) m.max_long_term_frame_idx_plus1 = (int)ue(&r);
                if (m.op == 5) o->has_mmco5 = 1;
                o->mmco[o->n_mmco++] = m;
            }
    }
    return r.err ? -1 : 0;
}

// ---- picture order count (8.2.1) ----
void h264_poc(H264PocState *st, const H264Sps *s, const H264Slice *sl, int *top, int *bottom)
{
    const int max_frame_num = 1 << (s->log2_max_frame_num_minus4 + 4);
    int t = 0, b = 0;
    if (s->pic_order_cnt_type == 0) {
        const int max_lsb = 1 << (s->log2_max_pic_order_cnt_lsb_minus4 + 4);
        int prev_msb, prev_lsb;
        if (sl->idr) prev_msb = prev_lsb = 0;
        else if (st->prev_ref_has_mmco5) { prev_msb = 0; prev_lsb = st->prev_ref_bottom_field ? 0 : st->prev_ref_top_poc; }
        else { prev_msb = st->prev_poc_msb; prev_lsb = st->prev_poc_lsb; }
        const int lsb = sl->pic_order_cnt_lsb;
        int msb = prev_msb;
        if (lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2) msb = prev_msb + max_lsb;
        else if (lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2) msb = prev_msb - max_lsb;
        if (!sl->bottom_field_flag) { t = msb + lsb; b = sl->field_pic_flag ? t : t + sl->delta_pic_order_cnt_bottom; }
        else t = b = msb + lsb;
        if (sl->nal_ref_idc) {
            st->prev_ref_has_mmco5 = sl->has_mmco5; st->prev_ref_bottom_field = sl->bottom_field_flag;
            if (sl->has_mmco5) { int m = t < b ? t : b; st->prev_ref_top_poc = t - m; st->prev_poc_msb = 0; st->prev_poc_lsb = st->prev_ref_top_poc; }
            else { st->prev_poc_msb = msb; st->prev_poc_lsb = lsb; }
        }
    } else {
        int fno;
        if (sl->idr) fno = 0;
        else if (st->prev_frame_num > sl->frame_num) fno = st->prev_frame_num_offset + max_frame_num;
        else fno = st->prev_frame_num_offset;
        if (s->pic_order_cnt_type == 1) {
            int n = s->num_ref_frames_in_pic_order_cnt_cycle, abs = n ? fno + sl->frame_num : 0, exp = 0, per_cycle = 0;
            if (!sl->nal_ref_idc && abs > 0) abs--;
            for (int i = 0; i < n; i++) per_cycle += s->offset_for_ref_frame[i];
            if (abs > 0) {
                int cyc = (abs - 1) / n, in = (abs - 1) % n;
                exp = cyc * per_cycle; for (int i = 0; i <= in; i++) exp += s->offset_for_ref_frame[i];
            }
            if (!sl->nal_ref_idc) exp += s->offset_for_non_ref_pic;
            if (!sl->field_pic_flag) { t = exp + sl->delta_pic_order_cnt[0]; b = t + s->offset_for_top_to_bottom_field + sl->delta_pic_order_cnt[1]; }
            else if (!sl->bottom_field_flag) t = b = exp + sl->delta_pic_order_cnt[0];
            else t = b = exp + s->offset_for_top_to_bottom_field + sl->delta_pic_order_cnt[0];
        } else {
            int tmp = sl->idr ? 0 : (sl->nal_ref_idc ? 2 * (fno + sl->frame_num) : 2 * (fno + sl->frame_num) - 1);
            t = b = tmp;
        }
        st->prev_frame_num_offset = sl->has_mmco5 ? 0 : fno;
        st->prev_frame_num = sl->has_mmco5 ? 0 : sl->frame_num;
    }
    *top = t; *bottom = b;
}

bool h264_new_picture(const H264Params *p, const H264Slice *a, const H264Slice *b)
{
    if (a->frame_num != b->frame_num || a->pps_id != b->pps_id || a->field_pic_flag != b->field_pic_flag) return true;
    if (a->field_pic_flag && a->bottom_field_flag != b->bottom_field_flag) return true;
    if ((a->nal_ref_idc == 0) != (b->nal_ref_idc == 0) || a->idr != b->idr) return true;
    if (a->idr && b->idr && a->idr_pic_id != b->idr_pic_id) return true;
    const H264Sps *s = &p->sps[p->pps[b->pps_id].sps_id];
    if (s->pic_order_cnt_type == 0 && (a->pic_order_cnt_lsb != b->pic_order_cnt_lsb || a->delta_pic_order_cnt_bottom != b->delta_pic_order_cnt_bottom)) return true;
    if (s->pic_order_cnt_type == 1 && (a->delta_pic_order_cnt[0] != b->delta_pic_order_cnt[0] || a->delta_pic_order_cnt[1] != b->delta_pic_order_cnt[1])) return true;
    return false;
}
