// hevcparse.c (M11): see hevcparse.h. Section numbers refer to ITU-T H.265.
#include "hevcparse.h"
#include <string.h>

// ---- bit reader over the RBSP (emulation prevention removed) ----
typedef struct { uint8_t buf[8192]; size_t n, bit; bool err; } Br;
static void br_init(Br *r, const uint8_t *nal, size_t len)   // skips the 2-byte NAL header
{
    r->n = 0; r->bit = 0; r->err = false;
    int zeros = 0;
    for (size_t i = 2; i < len && r->n < sizeof r->buf; i++) {
        if (zeros >= 2 && nal[i] == 3) { zeros = 0; continue; }
        zeros = nal[i] == 0 ? zeros + 1 : 0;
        r->buf[r->n++] = nal[i];
    }
}
static unsigned u1(Br *r) { if (r->bit >= r->n * 8) { r->err = true; return 0; } unsigned v = r->buf[r->bit >> 3] >> (7 - (r->bit & 7)) & 1; r->bit++; return v; }
static unsigned u(Br *r, int n) { unsigned v = 0; while (n-- > 0) v = v << 1 | u1(r); return v; }
static unsigned ue(Br *r) { int z = 0; while (!u1(r)) { if (++z > 31 || r->err) { r->err = true; return 0; } } return ((1u << z) - 1) + u(r, z); }
static int se(Br *r) { unsigned k = ue(r); return (k & 1) ? (int)((k + 1) >> 1) : -(int)(k >> 1); }
static int ceil_log2(unsigned x) { int k = 0; while ((1u << k) < x) k++; return k; }

// ---- NAL units ----
bool hevc_next_annexb(const uint8_t *b, size_t n, size_t *pos, const uint8_t **nal, size_t *len)
{
    size_t i = *pos;
    while (i + 3 <= n && !(b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1)) i++;
    if (i + 3 > n) return false;
    i += 3;
    size_t j = i;
    while (j + 3 <= n && !(b[j] == 0 && b[j + 1] == 0 && (b[j + 2] == 1 || (b[j + 2] == 0 && j + 3 < n && b[j + 3] == 1)))) j++;
    if (j + 3 > n) j = n;
    *nal = b + i; *len = j - i; *pos = j;
    while (*len > 0 && (*nal)[*len - 1] == 0) (*len)--;   // trailing zero bytes
    return *len > 0;
}
bool hevc_next_lenpfx(const uint8_t *b, size_t n, int nal_len, size_t *pos, const uint8_t **nal, size_t *len)
{
    if (*pos + (size_t)nal_len > n) return false;
    size_t l = 0; for (int k = 0; k < nal_len; k++) l = l << 8 | b[*pos + (size_t)k];
    *pos += (size_t)nal_len;
    if (*pos + l > n) return false;
    *nal = b + *pos; *len = l; *pos += l;
    return true;
}

// ---- scaling lists (7.3.4, 7.4.5) ----
static const uint8_t default_intra8[64] = {
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20, 21, 20, 19, 21, 24, 22, 22, 24,
    24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31, 29, 36, 41, 44, 41, 36, 47, 54, 54, 47, 65, 70, 65, 88, 88, 115 };
static const uint8_t default_inter8[64] = {
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20, 20, 20, 20, 20, 24, 24, 24, 24,
    24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28, 28, 33, 33, 33, 33, 33, 41, 41, 41, 41, 54, 54, 54, 71, 71, 91 };

static void default_list(HevcScaling *sl, int size, int m)
{
    if (size == 0) memset(sl->list[0][m], 16, 16);
    else memcpy(sl->list[size][m], m < 3 ? default_intra8 : default_inter8, 64);
    if (size >= 2) sl->dc[size - 2][m] = 16;
}
void hevc_default_scaling(HevcScaling *sl)
{
    memset(sl, 0, sizeof *sl);
    for (int s = 0; s < 4; s++) for (int m = 0; m < 6; m++) default_list(sl, s, m);
}
static void scaling_list_data(Br *r, HevcScaling *sl, int chroma_format_idc)
{
    for (int size = 0; size < 4; size++)
        for (int m = 0; m < 6; m += size == 3 ? 3 : 1) {
            const int n = size == 0 ? 16 : 64;
            if (!u1(r)) {                                          // scaling_list_pred_mode_flag
                const unsigned delta = ue(r);
                if (delta == 0) default_list(sl, size, m);
                else {
                    const int ref = m - (int)delta * (size == 3 ? 3 : 1);
                    if (ref < 0) { r->err = true; return; }
                    memcpy(sl->list[size][m], sl->list[size][ref], (size_t)n);
                    if (size >= 2) sl->dc[size - 2][m] = sl->dc[size - 2][ref];
                }
            } else {
                int next = 8;
                if (size > 1) { next = se(r) + 8; sl->dc[size - 2][m] = (uint8_t)next; }
                for (int i = 0; i < n; i++) { next = (next + se(r) + 256) % 256; sl->list[size][m][i] = (uint8_t)next; }
            }
        }
    // 32x32 chroma lists (4:4:4): from the 16x16 ones (7.4.5)
    for (int m = 1; m < 6; m++) if (m != 3) {
        memcpy(sl->list[3][m], sl->list[2][m], 64); sl->dc[1][m] = sl->dc[0][m];
    }
    (void)chroma_format_idc;
}

// ---- profile_tier_level (7.3.3), VUI / HRD (E.2) ----
static void profile_tier_level(Br *r, HevcSps *s, int max_sub)
{
    u(r, 2); u1(r); s->general_profile_idc = (int)u(r, 5);
    u(r, 32); u(r, 4); u(r, 32); u(r, 11);                     // compatibility flags, 4 source flags, 43 constraint bits
    u1(r);                                                     // general_inbld_flag / reserved
    s->general_level_idc = (int)u(r, 8);
    int prof[8] = { 0 }, lev[8] = { 0 };
    for (int i = 0; i < max_sub; i++) { prof[i] = (int)u1(r); lev[i] = (int)u1(r); }
    if (max_sub > 0) for (int i = max_sub; i < 8; i++) u(r, 2);
    for (int i = 0; i < max_sub; i++) {
        if (prof[i]) { u(r, 32); u(r, 32); u(r, 24); }       // 88 bits
        if (lev[i]) u(r, 8);
    }
}
static void sub_layer_hrd(Br *r, int cpb_cnt, int sub_pic)
{
    for (int j = 0; j <= cpb_cnt; j++) { ue(r); ue(r); if (sub_pic) { ue(r); ue(r); } u1(r); }
}
static void hrd_parameters(Br *r, int common, int max_sub)
{
    int nal = 0, vcl = 0, sub_pic = 0;
    if (common) {
        nal = (int)u1(r); vcl = (int)u1(r);
        if (nal || vcl) {
            sub_pic = (int)u1(r);
            if (sub_pic) { u(r, 8); u(r, 5); u1(r); u(r, 5); }
            u(r, 4); u(r, 4);
            if (sub_pic) u(r, 4);
            u(r, 5); u(r, 5); u(r, 5);
        }
    }
    for (int i = 0; i <= max_sub; i++) {
        int fixed_within = 1, low_delay = 0, cpb_cnt = 0;
        if (!u1(r)) fixed_within = (int)u1(r);                 // fixed_pic_rate_general_flag, within_cvs
        if (fixed_within) ue(r); else low_delay = (int)u1(r);
        if (!low_delay) cpb_cnt = (int)ue(r);
        if (nal) sub_layer_hrd(r, cpb_cnt, sub_pic);
        if (vcl) sub_layer_hrd(r, cpb_cnt, sub_pic);
    }
}
static void vui_parameters(Br *r, HevcSps *s)
{
    if (u1(r)) { if (u(r, 8) == 255) { u(r, 16); u(r, 16); } }   // aspect ratio
    if (u1(r)) u1(r);                                          // overscan
    if (u1(r)) {                                               // video signal type
        u(r, 3); s->video_full_range_flag = (int)u1(r);
        if (u1(r)) { s->colour_primaries = (int)u(r, 8); s->transfer_characteristics = (int)u(r, 8); s->matrix_coeffs = (int)u(r, 8); }
    }
    if (u1(r)) { ue(r); ue(r); }                               // chroma loc
    u1(r); u1(r); u1(r);                                       // neutral chroma, field_seq, frame_field_info
    if (u1(r)) { ue(r); ue(r); ue(r); ue(r); }                 // default display window
    if (u1(r)) {                                               // timing
        u(r, 32); u(r, 32);
        if (u1(r)) ue(r);
        if (u1(r)) hrd_parameters(r, 1, s->max_sub_layers_minus1);
    }
    if (u1(r)) { u1(r); u1(r); u1(r); ue(r); ue(r); ue(r); ue(r); ue(r); }   // bitstream restriction
}

// ---- short-term reference picture sets (7.3.7, 7.4.8) ----
static int st_ref_pic_set(Br *r, HevcStRps *sets, int idx, int num_sets, HevcStRps *out, int *ref_num_delta)
{
    HevcStRps rps = { 0 };
    int inter = idx != 0 ? (int)u1(r) : 0;
    if (inter) {
        int delta_idx = 1;
        if (idx == num_sets) delta_idx = (int)ue(r) + 1;
        if (delta_idx > idx) return -1;
        const HevcStRps *R = &sets[idx - delta_idx];
        if (ref_num_delta) *ref_num_delta = R->num_negative + R->num_positive;
        const int sign = (int)u1(r), deltaRps = (1 - 2 * sign) * ((int)ue(r) + 1);
        const int nd = R->num_negative + R->num_positive;
        int used[33], use_delta[33];
        for (int j = 0; j <= nd; j++) { used[j] = (int)u1(r); use_delta[j] = used[j] ? 1 : (int)u1(r); }
        int i = 0;
        for (int j = R->num_positive - 1; j >= 0; j--) {
            const int d = R->delta_poc[R->num_negative + j] + deltaRps;
            if (d < 0 && use_delta[R->num_negative + j]) { rps.delta_poc[i] = d; rps.used[i++] = used[R->num_negative + j]; }
        }
        if (deltaRps < 0 && use_delta[nd]) { rps.delta_poc[i] = deltaRps; rps.used[i++] = used[nd]; }
        for (int j = 0; j < R->num_negative; j++) {
            const int d = R->delta_poc[j] + deltaRps;
            if (d < 0 && use_delta[j]) { rps.delta_poc[i] = d; rps.used[i++] = used[j]; }
        }
        rps.num_negative = i;
        int s1[32], u1v[32], k = 0;
        for (int j = R->num_negative - 1; j >= 0; j--) {
            const int d = R->delta_poc[j] + deltaRps;
            if (d > 0 && use_delta[j]) { s1[k] = d; u1v[k++] = used[j]; }
        }
        if (deltaRps > 0 && use_delta[nd]) { s1[k] = deltaRps; u1v[k++] = used[nd]; }
        for (int j = 0; j < R->num_positive; j++) {
            const int d = R->delta_poc[R->num_negative + j] + deltaRps;
            if (d > 0 && use_delta[R->num_negative + j]) { s1[k] = d; u1v[k++] = used[R->num_negative + j]; }
        }
        rps.num_positive = k;
        if (i + k > 32) return -1;
        for (int j = 0; j < k; j++) { rps.delta_poc[i + j] = s1[j]; rps.used[i + j] = u1v[j]; }
    } else {
        rps.num_negative = (int)ue(r); rps.num_positive = (int)ue(r);
        if (rps.num_negative > 16 || rps.num_positive > 16) return -1;
        int poc = 0;
        for (int i = 0; i < rps.num_negative; i++) { poc -= (int)ue(r) + 1; rps.delta_poc[i] = poc; rps.used[i] = (int)u1(r); }
        poc = 0;
        for (int i = 0; i < rps.num_positive; i++) { poc += (int)ue(r) + 1; rps.delta_poc[rps.num_negative + i] = poc; rps.used[rps.num_negative + i] = (int)u1(r); }
    }
    *out = rps;
    return r->err ? -1 : 0;
}

// ---- SPS (7.3.2.2) ----
int hevc_parse_sps(HevcParams *p, const uint8_t *nal, size_t len)
{
    static Br r; br_init(&r, nal, len);
    HevcSps s; memset(&s, 0, sizeof s);
    s.vps_id = (int)u(&r, 4); s.max_sub_layers_minus1 = (int)u(&r, 3); u1(&r);
    profile_tier_level(&r, &s, s.max_sub_layers_minus1);
    s.sps_id = (int)ue(&r);
    if (s.sps_id > 15) return -1;
    s.chroma_format_idc = (int)ue(&r);
    if (s.chroma_format_idc == 3) s.separate_colour_plane_flag = (int)u1(&r);
    s.width = (int)ue(&r); s.height = (int)ue(&r);
    if (u1(&r)) {
        const int sw = (s.chroma_format_idc == 1 || s.chroma_format_idc == 2) ? 2 : 1, sh = s.chroma_format_idc == 1 ? 2 : 1;
        s.conf_win_left = (int)ue(&r) * sw; s.conf_win_right = (int)ue(&r) * sw;
        s.conf_win_top = (int)ue(&r) * sh; s.conf_win_bottom = (int)ue(&r) * sh;
    }
    s.bit_depth_luma = (int)ue(&r) + 8; s.bit_depth_chroma = (int)ue(&r) + 8;
    s.log2_max_poc_lsb = (int)ue(&r) + 4;
    const int ordering = (int)u1(&r);
    for (int i = ordering ? 0 : s.max_sub_layers_minus1; i <= s.max_sub_layers_minus1; i++) {
        s.max_dec_pic_buffering[i] = (int)ue(&r) + 1; s.max_num_reorder[i] = (int)ue(&r); s.max_latency_increase_plus1[i] = (int)ue(&r);
    }
    if (!ordering) for (int i = 0; i < s.max_sub_layers_minus1; i++) { s.max_dec_pic_buffering[i] = s.max_dec_pic_buffering[s.max_sub_layers_minus1]; s.max_num_reorder[i] = s.max_num_reorder[s.max_sub_layers_minus1]; s.max_latency_increase_plus1[i] = s.max_latency_increase_plus1[s.max_sub_layers_minus1]; }
    s.log2_min_cb = (int)ue(&r) + 3; s.log2_max_cb = s.log2_min_cb + (int)ue(&r);
    s.log2_min_tb = (int)ue(&r) + 2; s.log2_max_tb = s.log2_min_tb + (int)ue(&r);
    s.max_th_depth_inter = (int)ue(&r); s.max_th_depth_intra = (int)ue(&r);
    s.scaling_list_enabled = (int)u1(&r);
    hevc_default_scaling(&s.scaling);
    if (s.scaling_list_enabled) { s.scaling_list_data_present = (int)u1(&r); if (s.scaling_list_data_present) scaling_list_data(&r, &s.scaling, s.chroma_format_idc); }
    s.amp_enabled = (int)u1(&r); s.sao_enabled = (int)u1(&r); s.pcm_enabled = (int)u1(&r);
    if (s.pcm_enabled) {
        s.pcm_bit_depth_luma = (int)u(&r, 4) + 1; s.pcm_bit_depth_chroma = (int)u(&r, 4) + 1;
        s.log2_min_pcm_cb = (int)ue(&r) + 3; s.log2_max_pcm_cb = s.log2_min_pcm_cb + (int)ue(&r);
        s.pcm_loop_filter_disabled = (int)u1(&r);
    }
    s.num_short_term_ref_pic_sets = (int)ue(&r);
    if (s.num_short_term_ref_pic_sets > 64) return -1;
    for (int i = 0; i < s.num_short_term_ref_pic_sets; i++) if (st_ref_pic_set(&r, s.st_rps, i, s.num_short_term_ref_pic_sets, &s.st_rps[i], NULL)) return -1;
    s.long_term_ref_pics_present = (int)u1(&r);
    if (s.long_term_ref_pics_present) {
        s.num_long_term_ref_pics_sps = (int)ue(&r);
        if (s.num_long_term_ref_pics_sps > 32) return -1;
        for (int i = 0; i < s.num_long_term_ref_pics_sps; i++) { s.lt_ref_pic_poc_lsb_sps[i] = (int)u(&r, s.log2_max_poc_lsb); s.used_by_curr_pic_lt_sps[i] = (int)u1(&r); }
    }
    s.temporal_mvp_enabled = (int)u1(&r); s.strong_intra_smoothing_enabled = (int)u1(&r);
    s.colour_primaries = s.transfer_characteristics = s.matrix_coeffs = 2;
    s.vui_present = (int)u1(&r);
    if (s.vui_present) vui_parameters(&r, &s);
    bool ext_data = false;                                     // extensions we don't read: end_bit unknown
    if (u1(&r)) {                                              // sps_extension_present_flag
        const int range = (int)u1(&r), multilayer = (int)u1(&r), ext3d = (int)u1(&r), scc = (int)u1(&r), ext4 = (int)u(&r, 4);
        ext_data = ext3d || scc || ext4;
        if (range) {
            s.transform_skip_rotation_enabled = (int)u1(&r); s.transform_skip_context_enabled = (int)u1(&r);
            s.implicit_rdpcm_enabled = (int)u1(&r); s.explicit_rdpcm_enabled = (int)u1(&r);
            s.extended_precision_processing = (int)u1(&r); s.intra_smoothing_disabled = (int)u1(&r);
            s.high_precision_offsets_enabled = (int)u1(&r); s.persistent_rice_adaptation_enabled = (int)u1(&r);
            s.cabac_bypass_alignment_enabled = (int)u1(&r);
        }
        if (multilayer) u1(&r);                                // inter_view_mv_vert_constraint_flag (F.7.3.2.2.5)
    }
    if (r.err) return -1;
    s.end_bit = ext_data ? 0 : (int)r.bit; s.valid = true;
    p->sps[s.sps_id] = s;
    return s.sps_id;
}

// ---- PPS (7.3.2.3) ----
int hevc_parse_pps(HevcParams *p, const uint8_t *nal, size_t len)
{
    static Br r; br_init(&r, nal, len);
    HevcPps q; memset(&q, 0, sizeof q);
    q.pps_id = (int)ue(&r); q.sps_id = (int)ue(&r);
    if (q.pps_id > 63 || q.sps_id > 15) return -1;
    q.dependent_slice_segments_enabled = (int)u1(&r); q.output_flag_present = (int)u1(&r); q.num_extra_slice_header_bits = (int)u(&r, 3);
    q.sign_data_hiding_enabled = (int)u1(&r); q.cabac_init_present = (int)u1(&r);
    q.num_ref_idx_l0_default_active = (int)ue(&r) + 1; q.num_ref_idx_l1_default_active = (int)ue(&r) + 1;
    q.init_qp_minus26 = se(&r); q.constrained_intra_pred = (int)u1(&r); q.transform_skip_enabled = (int)u1(&r);
    q.cu_qp_delta_enabled = (int)u1(&r);
    if (q.cu_qp_delta_enabled) q.diff_cu_qp_delta_depth = (int)ue(&r);
    q.cb_qp_offset = se(&r); q.cr_qp_offset = se(&r); q.slice_chroma_qp_offsets_present = (int)u1(&r);
    q.weighted_pred = (int)u1(&r); q.weighted_bipred = (int)u1(&r); q.transquant_bypass_enabled = (int)u1(&r);
    q.tiles_enabled = (int)u1(&r); q.entropy_coding_sync_enabled = (int)u1(&r);
    q.num_tile_columns = q.num_tile_rows = 1; q.uniform_spacing = 1;
    if (q.tiles_enabled) {
        q.num_tile_columns = (int)ue(&r) + 1; q.num_tile_rows = (int)ue(&r) + 1;
        if (q.num_tile_columns > 20 || q.num_tile_rows > 22) return -1;
        q.uniform_spacing = (int)u1(&r);
        if (!q.uniform_spacing) {
            for (int i = 0; i < q.num_tile_columns - 1; i++) q.column_width[i] = (int)ue(&r) + 1;
            for (int i = 0; i < q.num_tile_rows - 1; i++) q.row_height[i] = (int)ue(&r) + 1;
        }
        q.loop_filter_across_tiles = (int)u1(&r);
    }
    q.loop_filter_across_slices = (int)u1(&r);
    q.deblocking_filter_control_present = (int)u1(&r);
    if (q.deblocking_filter_control_present) {
        q.deblocking_filter_override_enabled = (int)u1(&r); q.pps_deblocking_filter_disabled = (int)u1(&r);
        if (!q.pps_deblocking_filter_disabled) { q.beta_offset_div2 = se(&r); q.tc_offset_div2 = se(&r); }
    }
    q.scaling_list_data_present = (int)u1(&r);
    hevc_default_scaling(&q.scaling);
    if (q.scaling_list_data_present) scaling_list_data(&r, &q.scaling, p->sps[q.sps_id].chroma_format_idc);
    q.lists_modification_present = (int)u1(&r); q.log2_parallel_merge_level = (int)ue(&r) + 2;
    q.slice_segment_header_extension_present = (int)u1(&r);
    bool ext_data = false;
    if (u1(&r)) {                                              // pps_extension_present_flag
        const int range = (int)u1(&r), multilayer = (int)u1(&r), ext3d = (int)u1(&r), scc = (int)u1(&r), ext4 = (int)u(&r, 4);
        ext_data = multilayer || ext3d || scc || ext4;
        // Main / Main 10 streams may carry anything here (conformance PS_A): read the range extension for RExt and up only
        if (range && p->sps[q.sps_id].valid && p->sps[q.sps_id].general_profile_idc < 4) ext_data = true;
        else if (range) {
            if (q.transform_skip_enabled) q.log2_max_transform_skip_size = (int)ue(&r) + 2;
            q.cross_component_prediction_enabled = (int)u1(&r); q.chroma_qp_offset_list_enabled = (int)u1(&r);
            if (q.chroma_qp_offset_list_enabled) {
                q.diff_cu_chroma_qp_offset_depth = (int)ue(&r); q.chroma_qp_offset_list_len = (int)ue(&r) + 1;
                if (q.chroma_qp_offset_list_len > 6) return -1;
                for (int i = 0; i < q.chroma_qp_offset_list_len; i++) { q.cb_qp_offset_list[i] = se(&r); q.cr_qp_offset_list[i] = se(&r); }
            }
            q.log2_sao_offset_scale_luma = (int)ue(&r); q.log2_sao_offset_scale_chroma = (int)ue(&r);
        }
    }
    if (r.err) return -1;
    q.end_bit = ext_data ? 0 : (int)r.bit; q.valid = true;
    p->pps[q.pps_id] = q;
    return q.pps_id;
}

void hevc_tiles(const HevcSps *sps, const HevcPps *pps, int *cols, int *rows)
{
    const int ctb = 1 << sps->log2_max_cb;
    const int wc = (sps->width + ctb - 1) / ctb, hc = (sps->height + ctb - 1) / ctb;
    const int nc = pps->num_tile_columns, nr = pps->num_tile_rows;
    if (pps->uniform_spacing) {
        for (int i = 0; i < nc; i++) cols[i] = ((i + 1) * wc) / nc - (i * wc) / nc;
        for (int i = 0; i < nr; i++) rows[i] = ((i + 1) * hc) / nr - (i * hc) / nr;
    } else {
        int sum = 0; for (int i = 0; i < nc - 1; i++) { cols[i] = pps->column_width[i]; sum += cols[i]; } cols[nc - 1] = wc - sum;
        sum = 0; for (int i = 0; i < nr - 1; i++) { rows[i] = pps->row_height[i]; sum += rows[i]; } rows[nr - 1] = hc - sum;
    }
}

// ---- slice segment header (7.3.6) ----
static void pred_weight_table(Br *r, const HevcSps *sps, const HevcSlice *s)
{
    const int chroma = sps->chroma_format_idc != 0 && !sps->separate_colour_plane_flag;
    ue(r);
    if (chroma) se(r);
    for (int l = 0; l < (s->slice_type == HEVC_SLICE_B ? 2 : 1); l++) {
        int lf[16], cf[16] = { 0 };
        for (int i = 0; i < s->num_ref_idx_active[l]; i++) lf[i] = (int)u1(r);
        if (chroma) for (int i = 0; i < s->num_ref_idx_active[l]; i++) cf[i] = (int)u1(r);
        for (int i = 0; i < s->num_ref_idx_active[l]; i++) {
            if (lf[i]) { se(r); se(r); }
            if (cf[i]) for (int j = 0; j < 2; j++) { se(r); se(r); }
        }
    }
}

int hevc_parse_slice_header(const HevcParams *p, const uint8_t *nal, size_t len, HevcSlice *s)
{
    static Br r; br_init(&r, nal, len);
    memset(s, 0, sizeof *s);
    if (len < 2) return -1;
    s->nal_type = (nal[0] >> 1) & 63; s->temporal_id = (nal[1] & 7) - 1;
    s->first_slice_segment_in_pic = (int)u1(&r);
    if (hevc_is_irap(s->nal_type)) s->no_output_of_prior_pics = (int)u1(&r);
    s->pps_id = (int)ue(&r);
    if (s->pps_id > 63 || !p->pps[s->pps_id].valid) return -1;
    const HevcPps *pps = &p->pps[s->pps_id]; const HevcSps *sps = &p->sps[pps->sps_id];
    if (!sps->valid) return -1;
    const int ctb = 1 << sps->log2_max_cb;
    const int pic_ctbs = ((sps->width + ctb - 1) / ctb) * ((sps->height + ctb - 1) / ctb);
    if (!s->first_slice_segment_in_pic) {
        if (pps->dependent_slice_segments_enabled) s->dependent_slice_segment = (int)u1(&r);
        s->slice_segment_address = (int)u(&r, ceil_log2((unsigned)pic_ctbs));
    }
    s->pic_output_flag = 1;
    s->num_ref_idx_active[0] = pps->num_ref_idx_l0_default_active; s->num_ref_idx_active[1] = pps->num_ref_idx_l1_default_active;
    s->deblocking_disabled = pps->pps_deblocking_filter_disabled; s->beta_offset_div2 = pps->beta_offset_div2; s->tc_offset_div2 = pps->tc_offset_div2;
    s->loop_filter_across_slices = pps->loop_filter_across_slices; s->collocated_from_l0 = 1;
    if (!s->dependent_slice_segment) {
        for (int i = 0; i < pps->num_extra_slice_header_bits; i++) u1(&r);
        s->slice_type = (int)ue(&r);
        if (pps->output_flag_present) s->pic_output_flag = (int)u1(&r);
        if (sps->separate_colour_plane_flag) s->colour_plane_id = (int)u(&r, 2);
        int num_pic_total_curr = 0;
        if (!hevc_is_idr(s->nal_type)) {
            s->poc_lsb = (int)u(&r, sps->log2_max_poc_lsb);
            const size_t rps0 = r.bit;
            s->short_term_ref_pic_set_sps_flag = (int)u1(&r);
            if (!s->short_term_ref_pic_set_sps_flag) {
                const size_t b0 = r.bit;
                if (st_ref_pic_set(&r, (HevcStRps *)sps->st_rps, sps->num_short_term_ref_pic_sets, sps->num_short_term_ref_pic_sets, &s->st_rps, &s->st_rps_ref_num_delta)) return -1;
                s->st_rps_bits = (int)(r.bit - b0);
            } else {
                if (sps->num_short_term_ref_pic_sets > 1) s->short_term_ref_pic_set_idx = (int)u(&r, ceil_log2((unsigned)sps->num_short_term_ref_pic_sets));
                if (s->short_term_ref_pic_set_idx >= sps->num_short_term_ref_pic_sets) return -1;
                s->st_rps = sps->st_rps[s->short_term_ref_pic_set_idx];
            }
            if (sps->long_term_ref_pics_present) {
                if (sps->num_long_term_ref_pics_sps > 0) s->num_long_term_sps = (int)ue(&r);
                s->num_long_term_pics = (int)ue(&r);
                if (s->num_long_term_sps + s->num_long_term_pics > 32) return -1;
                for (int i = 0; i < s->num_long_term_sps + s->num_long_term_pics; i++) {
                    if (i < s->num_long_term_sps) {
                        int idx = 0;
                        if (sps->num_long_term_ref_pics_sps > 1) idx = (int)u(&r, ceil_log2((unsigned)sps->num_long_term_ref_pics_sps));
                        s->lt_poc_lsb[i] = sps->lt_ref_pic_poc_lsb_sps[idx]; s->lt_used[i] = sps->used_by_curr_pic_lt_sps[idx];
                    } else { s->lt_poc_lsb[i] = (int)u(&r, sps->log2_max_poc_lsb); s->lt_used[i] = (int)u1(&r); }
                    s->lt_msb_present[i] = (int)u1(&r);
                    if (s->lt_msb_present[i]) s->lt_delta_msb_cycle[i] = (int)ue(&r);
                }
            }
            s->rps_start = (int)rps0; s->rps_bits = (int)(r.bit - rps0);
            if (sps->temporal_mvp_enabled) s->slice_temporal_mvp_enabled = (int)u1(&r);
            for (int i = 0; i < s->st_rps.num_negative + s->st_rps.num_positive; i++) num_pic_total_curr += s->st_rps.used[i];
            for (int i = 0; i < s->num_long_term_sps + s->num_long_term_pics; i++) num_pic_total_curr += s->lt_used[i];
        }
        if (sps->sao_enabled) {
            s->sao_luma = (int)u1(&r);
            if (sps->chroma_format_idc != 0 && !sps->separate_colour_plane_flag) s->sao_chroma = (int)u1(&r);
        }
        if (s->slice_type == HEVC_SLICE_P || s->slice_type == HEVC_SLICE_B) {
            if (u1(&r)) {                                      // num_ref_idx_active_override_flag
                s->num_ref_idx_active[0] = (int)ue(&r) + 1;
                if (s->slice_type == HEVC_SLICE_B) s->num_ref_idx_active[1] = (int)ue(&r) + 1;
            }
            if (s->num_ref_idx_active[0] > 16 || s->num_ref_idx_active[1] > 16) return -1;
            if (pps->lists_modification_present && num_pic_total_curr > 1) {
                const int bits = ceil_log2((unsigned)num_pic_total_curr);
                for (int l = 0; l < (s->slice_type == HEVC_SLICE_B ? 2 : 1); l++) {
                    s->ref_pic_list_modification[l] = (int)u1(&r);
                    if (s->ref_pic_list_modification[l]) for (int i = 0; i < s->num_ref_idx_active[l]; i++) s->list_entry[l][i] = (int)u(&r, bits);
                }
            }
            if (s->slice_type == HEVC_SLICE_B) s->mvd_l1_zero = (int)u1(&r);
            if (pps->cabac_init_present) s->cabac_init = (int)u1(&r);
            if (s->slice_temporal_mvp_enabled) {
                if (s->slice_type == HEVC_SLICE_B) s->collocated_from_l0 = (int)u1(&r);
                if ((s->collocated_from_l0 && s->num_ref_idx_active[0] > 1) || (!s->collocated_from_l0 && s->num_ref_idx_active[1] > 1))
                    s->collocated_ref_idx = (int)ue(&r);
            }
            if ((pps->weighted_pred && s->slice_type == HEVC_SLICE_P) || (pps->weighted_bipred && s->slice_type == HEVC_SLICE_B))
                pred_weight_table(&r, sps, s);
            s->max_num_merge_cand = 5 - (int)ue(&r);
        }
        s->slice_qp_delta = se(&r);
        if (pps->slice_chroma_qp_offsets_present) { s->slice_cb_qp_offset = se(&r); s->slice_cr_qp_offset = se(&r); }
        if (pps->chroma_qp_offset_list_enabled) s->cu_chroma_qp_offset_enabled = (int)u1(&r);
        if (pps->deblocking_filter_override_enabled) s->deblocking_override = (int)u1(&r);
        if (s->deblocking_override) {
            s->deblocking_disabled = (int)u1(&r);
            if (!s->deblocking_disabled) { s->beta_offset_div2 = se(&r); s->tc_offset_div2 = se(&r); }
        }
        if (pps->loop_filter_across_slices && (s->sao_luma || s->sao_chroma || !s->deblocking_disabled)) s->loop_filter_across_slices = (int)u1(&r);
    }
    if (pps->tiles_enabled || pps->entropy_coding_sync_enabled) {
        s->num_entry_point_offsets = (int)ue(&r);
        if (s->num_entry_point_offsets > 0) {
            s->offset_len_minus1 = (int)ue(&r);
            if (s->offset_len_minus1 > 31) return -1;
            for (int i = 0; i < s->num_entry_point_offsets && !r.err; i++) u(&r, s->offset_len_minus1 + 1);
        }
    }
    if (pps->slice_segment_header_extension_present) { const unsigned l = ue(&r); for (unsigned i = 0; i < l; i++) u(&r, 8); }
    // byte_alignment(): a 1 then zeros
    if (!u1(&r)) return -1;
    while (r.bit & 7) u1(&r);
    s->header_bits = (int)r.bit;
    // the header's length in the NAL unit, counting emulation prevention bytes
    size_t rb = r.bit >> 3, i = 2; int zeros = 0;
    for (size_t k = 0; i < len && k < rb; i++) {
        if (zeros >= 2 && nal[i] == 3) { zeros = 0; continue; }
        zeros = nal[i] == 0 ? zeros + 1 : 0; k++;
    }
    s->header_bytes = (int)i;
    return r.err ? -1 : 0;
}

// ---- picture order count (8.3.1) ----
int hevc_poc(HevcPocState *st, const HevcSps *sps, const HevcSlice *s, bool no_rasl_output)
{
    const int max_lsb = 1 << sps->log2_max_poc_lsb;
    int msb = 0;
    if (!(hevc_is_irap(s->nal_type) && no_rasl_output)) {
        const int prev_lsb = st->prev_poc_tid0 & (max_lsb - 1), prev_msb = st->prev_poc_tid0 - prev_lsb;
        if (s->poc_lsb < prev_lsb && prev_lsb - s->poc_lsb >= max_lsb / 2) msb = prev_msb + max_lsb;
        else if (s->poc_lsb > prev_lsb && s->poc_lsb - prev_lsb > max_lsb / 2) msb = prev_msb - max_lsb;
        else msb = prev_msb;
    }
    const int poc = msb + s->poc_lsb;
    // prevTid0Pic: TemporalId 0 and not a RASL, RADL or sub-layer non-reference picture
    const int t = s->nal_type;
    const bool rasl_radl = t >= 6 && t <= 9, slnr = t <= 14 && (t & 1) == 0;
    if (s->temporal_id == 0 && !rasl_radl && !slnr) st->prev_poc_tid0 = poc;
    return poc;
}

