// av1parse.c (M9): see av1parse.h. Section numbers refer to the AV1 specification.
#include "av1parse.h"
#include <string.h>

enum { LAST_FRAME = 1, LAST2_FRAME, LAST3_FRAME, GOLDEN_FRAME, BWDREF_FRAME, ALTREF2_FRAME, ALTREF_FRAME };
enum { SEG_LVL_ALT_Q = 0, SEG_LVL_REF_FRAME = 5, SEG_LVL_MAX = 8, MAX_SEGMENTS = 8 };
enum { WARPEDMODEL_PREC_BITS = 16, GM_ABS_ALPHA_BITS = 12, GM_ALPHA_PREC_BITS = 15, GM_ABS_TRANS_ONLY_BITS = 9,
       GM_TRANS_ONLY_PREC_BITS = 3, GM_ABS_TRANS_BITS = 12, GM_TRANS_PREC_BITS = 6 };
enum { MAX_TILE_WIDTH = 4096, MAX_TILE_AREA = 4096 * 2304, MAX_TILE_ROWS = 64, MAX_TILE_COLS = 64 };
enum { SELECT_SCREEN_CONTENT_TOOLS = 2, SELECT_INTEGER_MV = 2 };

// ---- bit reader (4.10) ----
typedef struct { const uint8_t *b; size_t n, bit; bool err; } Br;
static unsigned f1(Br *r) { if (r->bit >= r->n * 8) { r->err = true; return 0; } unsigned v = r->b[r->bit >> 3] >> (7 - (r->bit & 7)) & 1; r->bit++; return v; }
static uint32_t f(Br *r, int n) { uint32_t v = 0; while (n-- > 0) v = v << 1 | f1(r); return v; }
static int su(Br *r, int n) { int v = (int)f(r, n), m = 1 << (n - 1); return (v & m) ? v - 2 * m : v; }
static uint32_t uvlc(Br *r)
{
    int lz = 0;
    while (!f1(r)) { if (++lz > 32 || r->err) { r->err = true; return 0; } }
    if (lz >= 32) return 0xffffffffu;
    return f(r, lz) + (uint32_t)((1ull << lz) - 1);
}
static int floor_log2(uint32_t x) { int s = 0; while (x > 1) { x >>= 1; s++; } return s; }
static int ns(Br *r, int n)
{
    const int w = floor_log2((uint32_t)n) + 1, m = (1 << w) - n;
    const int v = (int)f(r, w - 1);
    if (v < m) return v;
    return (v << 1) - m + (int)f1(r);
}
static void byte_align(Br *r) { while (r->bit & 7) f1(r); }

static uint64_t leb128(const uint8_t *p, size_t n, size_t *len)
{
    uint64_t v = 0; *len = 0;
    for (int i = 0; i < 8; i++) {
        if (*len >= n) { *len = 0; return 0; }
        const uint8_t b = p[(*len)++];
        v |= (uint64_t)(b & 0x7f) << (i * 7);
        if (!(b & 0x80)) break;
    }
    return v;
}

int av1_obu(const uint8_t *p, size_t n, Av1Obu *o)
{
    memset(o, 0, sizeof *o);
    if (n < 1 || (p[0] & 0x80)) return -1;
    o->type = (p[0] >> 3) & 15; o->extension = (p[0] >> 2) & 1; o->has_size = (p[0] >> 1) & 1;
    size_t h = 1;
    if (o->extension) { if (n < 2) return -1; o->temporal_id = p[1] >> 5; o->spatial_id = (p[1] >> 3) & 3; h = 2; }
    if (o->has_size) {
        size_t l; const uint64_t sz = leb128(p + h, n - h, &l);
        if (!l || h + l + sz > n) return -1;
        h += l; o->payload_len = (size_t)sz;
    } else o->payload_len = n - h;
    o->header_len = h;
    return 0;
}

// ---- sequence header (5.5) ----
int av1_parse_sequence_header(Av1Seq *s, const uint8_t *p, size_t n)
{
    Br r = { p, n, 0, false };
    memset(s, 0, sizeof *s);
    s->profile = (int)f(&r, 3); s->still_picture = (int)f1(&r); s->reduced_still_picture_header = (int)f1(&r);
    if (s->reduced_still_picture_header) {
        s->seq_level_idx[0] = (int)f(&r, 5);
    } else {
        s->timing_info_present = (int)f1(&r);
        if (s->timing_info_present) {
            f(&r, 32); f(&r, 32);                                   // num_units_in_display_tick, time_scale
            s->equal_picture_interval = (int)f1(&r);
            if (s->equal_picture_interval) uvlc(&r);
            s->decoder_model_info_present = (int)f1(&r);
            if (s->decoder_model_info_present) {
                const int bdl = (int)f(&r, 5); (void)bdl;
                f(&r, 32);
                s->buffer_removal_time_length_minus_1 = (int)f(&r, 5);
                s->frame_presentation_time_length_minus_1 = (int)f(&r, 5);
                s->buffer_removal_time_length_minus_1 |= bdl << 8;  // keep buffer_delay_length_minus_1 (bits 8+)
            }
        }
        const int idd = (int)f1(&r);                                // initial_display_delay_present_flag
        s->operating_points_cnt_minus_1 = (int)f(&r, 5);
        for (int i = 0; i <= s->operating_points_cnt_minus_1; i++) {
            s->operating_point_idc[i] = (int)f(&r, 12);
            s->seq_level_idx[i] = (int)f(&r, 5);
            if (s->seq_level_idx[i] > 7) f1(&r);                    // seq_tier
            if (s->decoder_model_info_present) {
                s->decoder_model_present_for_this_op[i] = (int)f1(&r);
                if (s->decoder_model_present_for_this_op[i]) {
                    const int nb = (s->buffer_removal_time_length_minus_1 >> 8) + 1;
                    f(&r, nb); f(&r, nb); f1(&r);                   // decoder/encoder_buffer_delay, low_delay_mode_flag
                }
            }
            if (idd && f1(&r)) f(&r, 4);
        }
        s->buffer_removal_time_length_minus_1 &= 0xff;
    }
    s->frame_width_bits_minus_1 = (int)f(&r, 4); s->frame_height_bits_minus_1 = (int)f(&r, 4);
    s->max_frame_width_minus_1 = (int)f(&r, s->frame_width_bits_minus_1 + 1);
    s->max_frame_height_minus_1 = (int)f(&r, s->frame_height_bits_minus_1 + 1);
    s->frame_id_numbers_present = s->reduced_still_picture_header ? 0 : (int)f1(&r);
    if (s->frame_id_numbers_present) { s->delta_frame_id_length_minus_2 = (int)f(&r, 4); s->additional_frame_id_length_minus_1 = (int)f(&r, 3); }
    s->use_128x128_superblock = (int)f1(&r); s->enable_filter_intra = (int)f1(&r); s->enable_intra_edge_filter = (int)f1(&r);
    if (s->reduced_still_picture_header) {
        s->seq_force_screen_content_tools = SELECT_SCREEN_CONTENT_TOOLS; s->seq_force_integer_mv = SELECT_INTEGER_MV;
    } else {
        s->enable_interintra_compound = (int)f1(&r); s->enable_masked_compound = (int)f1(&r);
        s->enable_warped_motion = (int)f1(&r); s->enable_dual_filter = (int)f1(&r); s->enable_order_hint = (int)f1(&r);
        if (s->enable_order_hint) { s->enable_jnt_comp = (int)f1(&r); s->enable_ref_frame_mvs = (int)f1(&r); }
        if (f1(&r)) s->seq_force_screen_content_tools = SELECT_SCREEN_CONTENT_TOOLS;   // seq_choose_screen_content_tools
        else s->seq_force_screen_content_tools = (int)f1(&r);
        if (s->seq_force_screen_content_tools > 0) {
            if (f1(&r)) s->seq_force_integer_mv = SELECT_INTEGER_MV;                    // seq_choose_integer_mv
            else s->seq_force_integer_mv = (int)f1(&r);
        } else s->seq_force_integer_mv = SELECT_INTEGER_MV;
        if (s->enable_order_hint) s->order_hint_bits = (int)f(&r, 3) + 1;
    }
    s->enable_superres = (int)f1(&r); s->enable_cdef = (int)f1(&r); s->enable_restoration = (int)f1(&r);
    // color_config (5.5.2)
    const int high = (int)f1(&r);
    if (s->profile == 2 && high) s->bit_depth = f1(&r) ? 12 : 10;
    else s->bit_depth = high ? 10 : 8;
    s->mono_chrome = s->profile == 1 ? 0 : (int)f1(&r);
    s->color_description_present = (int)f1(&r);
    if (s->color_description_present) { s->color_primaries = (int)f(&r, 8); s->transfer_characteristics = (int)f(&r, 8); s->matrix_coefficients = (int)f(&r, 8); }
    else { s->color_primaries = 2; s->transfer_characteristics = 2; s->matrix_coefficients = 2; }
    if (s->mono_chrome) {
        s->color_range = (int)f1(&r); s->subsampling_x = s->subsampling_y = 1;
    } else if (s->color_primaries == 1 && s->transfer_characteristics == 13 && s->matrix_coefficients == 0) {
        s->color_range = 1;
    } else {
        s->color_range = (int)f1(&r);
        if (s->profile == 0) s->subsampling_x = s->subsampling_y = 1;
        else if (s->profile == 1) s->subsampling_x = s->subsampling_y = 0;
        else if (s->bit_depth == 12) { s->subsampling_x = (int)f1(&r); s->subsampling_y = s->subsampling_x ? (int)f1(&r) : 0; }
        else { s->subsampling_x = 1; s->subsampling_y = 0; }
        if (s->subsampling_x && s->subsampling_y) s->chroma_sample_position = (int)f(&r, 2);
    }
    if (!s->mono_chrome) s->separate_uv_delta_q = (int)f1(&r);
    s->film_grain_params_present = (int)f1(&r);
    s->valid = !r.err;
    return r.err ? -1 : 0;
}

// ---- frame header (5.9) ----
static void superres_params(Br *r, const Av1Seq *s, Av1Frame *h)
{
    h->use_superres = s->enable_superres ? (int)f1(r) : 0;
    h->superres_denom = h->use_superres ? (int)f(r, 3) + 9 : 8;
    h->upscaled_width = h->frame_width;
    h->frame_width = (h->upscaled_width * 8 + h->superres_denom / 2) / h->superres_denom;
}
static void compute_image_size(Av1Frame *h) { h->mi_cols = 2 * ((h->frame_width + 7) >> 3); h->mi_rows = 2 * ((h->frame_height + 7) >> 3); }
static void frame_size(Br *r, const Av1Seq *s, Av1Frame *h)
{
    if (h->frame_size_override_flag) {
        h->frame_width = (int)f(r, s->frame_width_bits_minus_1 + 1) + 1;
        h->frame_height = (int)f(r, s->frame_height_bits_minus_1 + 1) + 1;
    } else { h->frame_width = s->max_frame_width_minus_1 + 1; h->frame_height = s->max_frame_height_minus_1 + 1; }
    superres_params(r, s, h);
    compute_image_size(h);
}
static void render_size(Br *r, Av1Frame *h)
{
    if (f1(r)) { h->render_width = (int)f(r, 16) + 1; h->render_height = (int)f(r, 16) + 1; }
    else { h->render_width = h->upscaled_width; h->render_height = h->frame_height; }
}
static int read_delta_q(Br *r) { return f1(r) ? su(r, 7) : 0; }

static void set_frame_refs(const Av1Seq *s, const Av1State *st, Av1Frame *h, int last_idx, int gold_idx)   // 7.8
{
    int used[8] = { 0 }, shifted[8];
    for (int i = 0; i < 7; i++) h->ref_frame_idx[i] = -1;
    h->ref_frame_idx[LAST_FRAME - LAST_FRAME] = last_idx; h->ref_frame_idx[GOLDEN_FRAME - LAST_FRAME] = gold_idx;
    used[last_idx] = used[gold_idx] = 1;
    const int cur = 1 << (s->order_hint_bits - 1);
    for (int i = 0; i < 8; i++) shifted[i] = cur + av1_get_relative_dist(s, st->ref[i].order_hint, h->order_hint);
    {   // ALTREF: latest backward
        int ref = -1, best = 0;
        for (int i = 0; i < 8; i++) if (!used[i] && shifted[i] >= cur && (ref < 0 || shifted[i] >= best)) { ref = i; best = shifted[i]; }
        if (ref >= 0) { h->ref_frame_idx[ALTREF_FRAME - LAST_FRAME] = ref; used[ref] = 1; }
    }
    for (int k = 0; k < 2; k++) {   // BWDREF, then ALTREF2: earliest backward
        int ref = -1, best = 0;
        for (int i = 0; i < 8; i++) if (!used[i] && shifted[i] >= cur && (ref < 0 || shifted[i] < best)) { ref = i; best = shifted[i]; }
        if (ref >= 0) { h->ref_frame_idx[(k ? ALTREF2_FRAME : BWDREF_FRAME) - LAST_FRAME] = ref; used[ref] = 1; }
    }
    static const int list[5] = { LAST2_FRAME, LAST3_FRAME, BWDREF_FRAME, ALTREF2_FRAME, ALTREF_FRAME };
    for (int k = 0; k < 5; k++) {
        if (h->ref_frame_idx[list[k] - LAST_FRAME] >= 0) continue;
        int ref = -1, best = 0;   // latest forward
        for (int i = 0; i < 8; i++) if (!used[i] && shifted[i] < cur && (ref < 0 || shifted[i] >= best)) { ref = i; best = shifted[i]; }
        if (ref >= 0) { h->ref_frame_idx[list[k] - LAST_FRAME] = ref; used[ref] = 1; }
    }
    int ref = -1, best = 0;
    for (int i = 0; i < 8; i++) if (ref < 0 || shifted[i] < best) { ref = i; best = shifted[i]; }
    for (int i = 0; i < 7; i++) if (h->ref_frame_idx[i] < 0) h->ref_frame_idx[i] = ref;
}

static int inverse_recenter(int r, int v)
{
    if (v > 2 * r) return v;
    return (v & 1) ? r - ((v + 1) >> 1) : r + (v >> 1);
}
static int decode_subexp(Br *r, int num)
{
    int i = 0, mk = 0;
    const int k = 3;
    for (;;) {
        const int b2 = i ? k + i - 1 : k, a = 1 << b2;
        if (num <= mk + 3 * a) return ns(r, num - mk) + mk;
        if (f1(r)) { i++; mk += a; } else return (int)f(r, b2) + mk;
        if (r->err) return 0;
    }
}
static int decode_unsigned_subexp_with_ref(Br *r, int mx, int ref)
{
    const int v = decode_subexp(r, mx);
    return (ref << 1) <= mx ? inverse_recenter(ref, v) : mx - 1 - inverse_recenter(mx - 1 - ref, v);
}
static int decode_signed_subexp_with_ref(Br *r, int low, int high, int ref) { return decode_unsigned_subexp_with_ref(r, high - low, ref - low) + low; }

static void read_global_param(Br *r, Av1Frame *h, const int prev[8][6], int type, int ref, int idx)
{
    int absBits = GM_ABS_ALPHA_BITS, precBits = GM_ALPHA_PREC_BITS;
    if (idx < 2) {
        if (type == AV1_TRANSLATION) {
            absBits = GM_ABS_TRANS_ONLY_BITS - !h->allow_high_precision_mv; precBits = GM_TRANS_ONLY_PREC_BITS - !h->allow_high_precision_mv;
        } else { absBits = GM_ABS_TRANS_BITS; precBits = GM_TRANS_PREC_BITS; }
    }
    const int precDiff = WARPEDMODEL_PREC_BITS - precBits;
    const int round = (idx % 3) == 2 ? (1 << WARPEDMODEL_PREC_BITS) : 0, sub = (idx % 3) == 2 ? (1 << precBits) : 0;
    const int mx = 1 << absBits, rr = (prev[ref][idx] >> precDiff) - sub;
    h->gm_params[ref][idx] = (decode_signed_subexp_with_ref(r, -mx, mx + 1, rr) * (1 << precDiff)) + round;
}

static void film_grain_params(Br *r, const Av1Seq *s, const Av1State *st, Av1Frame *h)   // 5.9.30
{
    Av1FilmGrain *g = &h->fg;
    memset(g, 0, sizeof *g);
    if (!s->film_grain_params_present || (!h->show_frame && !h->showable_frame)) return;
    g->apply_grain = (int)f1(r);
    if (!g->apply_grain) return;
    g->grain_seed = (int)f(r, 16);
    g->update_grain = h->frame_type == AV1_INTER_FRAME ? (int)f1(r) : 1;
    if (!g->update_grain) {
        const int idx = (int)f(r, 3), seed = g->grain_seed;
        *g = st->ref[idx].fg;                                       // load_grain_params
        g->grain_seed = seed; g->film_grain_params_ref_idx = idx; g->update_grain = 0;
        return;
    }
    g->num_y_points = (int)f(r, 4);
    for (int i = 0; i < g->num_y_points && i < 16; i++) { g->point_y_value[i] = (int)f(r, 8); g->point_y_scaling[i] = (int)f(r, 8); }
    g->chroma_scaling_from_luma = s->mono_chrome ? 0 : (int)f1(r);
    if (s->mono_chrome || g->chroma_scaling_from_luma || (s->subsampling_x == 1 && s->subsampling_y == 1 && g->num_y_points == 0)) {
        g->num_cb_points = g->num_cr_points = 0;
    } else {
        g->num_cb_points = (int)f(r, 4);
        for (int i = 0; i < g->num_cb_points && i < 16; i++) { g->point_cb_value[i] = (int)f(r, 8); g->point_cb_scaling[i] = (int)f(r, 8); }
        g->num_cr_points = (int)f(r, 4);
        for (int i = 0; i < g->num_cr_points && i < 16; i++) { g->point_cr_value[i] = (int)f(r, 8); g->point_cr_scaling[i] = (int)f(r, 8); }
    }
    g->grain_scaling_minus_8 = (int)f(r, 2);
    g->ar_coeff_lag = (int)f(r, 2);
    const int numPosLuma = 2 * g->ar_coeff_lag * (g->ar_coeff_lag + 1);
    int numPosChroma = numPosLuma;
    if (g->num_y_points) { numPosChroma = numPosLuma + 1; for (int i = 0; i < numPosLuma; i++) g->ar_coeffs_y_plus_128[i] = (int)f(r, 8); }
    if (g->chroma_scaling_from_luma || g->num_cb_points) for (int i = 0; i < numPosChroma; i++) g->ar_coeffs_cb_plus_128[i] = (int)f(r, 8);
    if (g->chroma_scaling_from_luma || g->num_cr_points) for (int i = 0; i < numPosChroma; i++) g->ar_coeffs_cr_plus_128[i] = (int)f(r, 8);
    g->ar_coeff_shift_minus_6 = (int)f(r, 2); g->grain_scale_shift = (int)f(r, 2);
    if (g->num_cb_points) { g->cb_mult = (int)f(r, 8); g->cb_luma_mult = (int)f(r, 8); g->cb_offset = (int)f(r, 9); }
    if (g->num_cr_points) { g->cr_mult = (int)f(r, 8); g->cr_luma_mult = (int)f(r, 8); g->cr_offset = (int)f(r, 9); }
    g->overlap_flag = (int)f1(r); g->clip_to_restricted_range = (int)f1(r);
}

static int tile_log2(int blk, int target) { int k = 0; while ((blk << k) < target) k++; return k; }

static int get_qindex(const Av1Frame *h, int seg)   // ignoreDeltaQ = 1
{
    if (h->seg_enabled && h->seg_feature_enabled[seg][SEG_LVL_ALT_Q]) {
        const int q = h->base_q_idx + h->seg_feature_data[seg][SEG_LVL_ALT_Q];
        return q < 0 ? 0 : q > 255 ? 255 : q;
    }
    return h->base_q_idx;
}

int av1_parse_frame_header(Av1State *st, const uint8_t *p, size_t n, Av1Frame *h, size_t *bytes)
{
    const Av1Seq *s = &st->seq;
    if (!s->valid) return -1;
    Br r = { p, n, 0, false };
    memset(h, 0, sizeof *h);
    h->temporal_id = st->temporal_id; h->spatial_id = st->spatial_id;
    const int idLen = s->frame_id_numbers_present ? s->additional_frame_id_length_minus_1 + s->delta_frame_id_length_minus_2 + 3 : 0;
    const int allFrames = 0xff;
    if (s->reduced_still_picture_header) {
        h->frame_type = AV1_KEY_FRAME; h->frame_is_intra = 1; h->show_frame = 1;
    } else {
        h->show_existing_frame = (int)f1(&r);
        if (h->show_existing_frame) {
            h->frame_to_show_map_idx = (int)f(&r, 3);
            if (s->decoder_model_info_present && !s->equal_picture_interval) f(&r, s->frame_presentation_time_length_minus_1 + 1);
            if (s->frame_id_numbers_present) f(&r, idLen);
            h->frame_type = st->ref[h->frame_to_show_map_idx].frame_type;
            h->refresh_frame_flags = h->frame_type == AV1_KEY_FRAME ? allFrames : 0;
            if (s->film_grain_params_present) h->fg = st->ref[h->frame_to_show_map_idx].fg;
            h->show_frame = 1;
            byte_align(&r); *bytes = r.bit >> 3;
            return r.err ? -1 : 0;
        }
        h->frame_type = (int)f(&r, 2);
        h->frame_is_intra = h->frame_type == AV1_INTRA_ONLY_FRAME || h->frame_type == AV1_KEY_FRAME;
        h->show_frame = (int)f1(&r);
        if (h->show_frame && s->decoder_model_info_present && !s->equal_picture_interval) f(&r, s->frame_presentation_time_length_minus_1 + 1);
        h->showable_frame = h->show_frame ? h->frame_type != AV1_KEY_FRAME : (int)f1(&r);
        if (h->frame_type == AV1_SWITCH_FRAME || (h->frame_type == AV1_KEY_FRAME && h->show_frame)) h->error_resilient_mode = 1;
        else h->error_resilient_mode = (int)f1(&r);
    }
    if (h->frame_type == AV1_KEY_FRAME && h->show_frame)
        for (int i = 0; i < 8; i++) { st->ref[i].valid = 0; st->ref[i].order_hint = 0; }
    h->disable_cdf_update = (int)f1(&r);
    h->allow_screen_content_tools = s->seq_force_screen_content_tools == SELECT_SCREEN_CONTENT_TOOLS ? (int)f1(&r) : s->seq_force_screen_content_tools;
    if (h->allow_screen_content_tools) h->force_integer_mv = s->seq_force_integer_mv == SELECT_INTEGER_MV ? (int)f1(&r) : s->seq_force_integer_mv;
    if (h->frame_is_intra) h->force_integer_mv = 1;
    if (s->frame_id_numbers_present) h->current_frame_id = (int)f(&r, idLen);
    if (h->frame_type == AV1_SWITCH_FRAME) h->frame_size_override_flag = 1;
    else if (s->reduced_still_picture_header) h->frame_size_override_flag = 0;
    else h->frame_size_override_flag = (int)f1(&r);
    h->order_hint = (int)f(&r, s->order_hint_bits);
    h->primary_ref_frame = (h->frame_is_intra || h->error_resilient_mode) ? AV1_PRIMARY_REF_NONE : (int)f(&r, 3);
    if (s->decoder_model_info_present) {
        if (f1(&r)) {                                               // buffer_removal_time_present_flag
            for (int op = 0; op <= s->operating_points_cnt_minus_1; op++) {
                if (!s->decoder_model_present_for_this_op[op]) continue;
                const int idc = s->operating_point_idc[op];
                const int inT = (idc >> h->temporal_id) & 1, inS = (idc >> (h->spatial_id + 8)) & 1;
                if (idc == 0 || (inT && inS)) f(&r, s->buffer_removal_time_length_minus_1 + 1);
            }
        }
    }
    if (h->frame_type == AV1_SWITCH_FRAME || (h->frame_type == AV1_KEY_FRAME && h->show_frame)) h->refresh_frame_flags = allFrames;
    else h->refresh_frame_flags = (int)f(&r, 8);
    if ((!h->frame_is_intra || h->refresh_frame_flags != allFrames) && h->error_resilient_mode && s->enable_order_hint)
        for (int i = 0; i < 8; i++) {
            const int roh = (int)f(&r, s->order_hint_bits);
            if (roh != st->ref[i].order_hint) { st->ref[i].valid = 0; st->ref[i].order_hint = roh; }   // 7.20 note: hint replaced
        }
    if (h->frame_is_intra) {
        frame_size(&r, s, h); render_size(&r, h);
        if (h->allow_screen_content_tools && h->upscaled_width == h->frame_width) h->allow_intrabc = (int)f1(&r);
    } else {
        if (s->enable_order_hint) {
            h->frame_refs_short_signaling = (int)f1(&r);
            if (h->frame_refs_short_signaling) { const int l = (int)f(&r, 3), g = (int)f(&r, 3); set_frame_refs(s, st, h, l, g); }
        }
        for (int i = 0; i < 7; i++) {
            if (!h->frame_refs_short_signaling) h->ref_frame_idx[i] = (int)f(&r, 3);
            if (s->frame_id_numbers_present) f(&r, s->delta_frame_id_length_minus_2 + 2);
        }
        if (h->frame_size_override_flag && !h->error_resilient_mode) {   // frame_size_with_refs (5.9.7)
            int found = 0;
            for (int i = 0; i < 7 && !found; i++) if (f1(&r)) {
                const Av1Ref *rf = &st->ref[h->ref_frame_idx[i]];
                found = 1;
                h->upscaled_width = rf->upscaled_width; h->frame_width = h->upscaled_width; h->frame_height = rf->frame_height;
                h->render_width = rf->render_width; h->render_height = rf->render_height;
            }
            if (!found) { frame_size(&r, s, h); render_size(&r, h); }
            else { superres_params(&r, s, h); compute_image_size(h); }
        } else { frame_size(&r, s, h); render_size(&r, h); }
        h->allow_high_precision_mv = h->force_integer_mv ? 0 : (int)f1(&r);
        h->interpolation_filter = f1(&r) ? AV1_SWITCHABLE : (int)f(&r, 2);
        h->is_motion_mode_switchable = (int)f1(&r);
        h->use_ref_frame_mvs = (h->error_resilient_mode || !s->enable_ref_frame_mvs) ? 0 : (int)f1(&r);
        for (int i = 0; i < 7; i++) {
            const int rf = LAST_FRAME + i, hint = st->ref[h->ref_frame_idx[i]].order_hint;
            h->order_hints[rf] = hint;
            h->ref_frame_sign_bias[rf] = s->enable_order_hint ? av1_get_relative_dist(s, hint, h->order_hint) > 0 : 0;
        }
    }
    h->disable_frame_end_update_cdf = (s->reduced_still_picture_header || h->disable_cdf_update) ? 1 : (int)f1(&r);
    // setup_past_independence / load_previous (the parts the header needs)
    int prevGm[8][6];
    if (h->primary_ref_frame == AV1_PRIMARY_REF_NONE) {
        for (int rf = 0; rf < 8; rf++) for (int i = 0; i < 6; i++) prevGm[rf][i] = (i % 3 == 2) ? 1 << WARPEDMODEL_PREC_BITS : 0;
        h->lf_delta_enabled = 1;
        static const int d[8] = { 1, 0, 0, 0, -1, 0, -1, -1 };
        memcpy(h->lf_ref_deltas, d, sizeof d);
    } else {
        const Av1Ref *pr = &st->ref[h->ref_frame_idx[h->primary_ref_frame]];
        memcpy(prevGm, pr->gm_params, sizeof prevGm);
        memcpy(h->lf_ref_deltas, pr->lf_ref_deltas, sizeof h->lf_ref_deltas); memcpy(h->lf_mode_deltas, pr->lf_mode_deltas, sizeof h->lf_mode_deltas);
        memcpy(h->seg_feature_enabled, pr->seg_feature_enabled, sizeof h->seg_feature_enabled);
        memcpy(h->seg_feature_data, pr->seg_feature_data, sizeof h->seg_feature_data);
    }
    // tile info (5.9.15)
    {
        const int sb128 = s->use_128x128_superblock;
        const int sbCols = sb128 ? (h->mi_cols + 31) >> 5 : (h->mi_cols + 15) >> 4, sbRows = sb128 ? (h->mi_rows + 31) >> 5 : (h->mi_rows + 15) >> 4;
        const int sbShift = sb128 ? 5 : 4, sbSize = sbShift + 2;
        const int maxTileWidthSb = MAX_TILE_WIDTH >> sbSize;
        int maxTileAreaSb = MAX_TILE_AREA >> (2 * sbSize);
        const int minLog2TileCols = tile_log2(maxTileWidthSb, sbCols);
        const int maxLog2TileCols = tile_log2(1, sbCols < MAX_TILE_COLS ? sbCols : MAX_TILE_COLS);
        const int maxLog2TileRows = tile_log2(1, sbRows < MAX_TILE_ROWS ? sbRows : MAX_TILE_ROWS);
        const int t = tile_log2(maxTileAreaSb, sbRows * sbCols), minLog2Tiles = minLog2TileCols > t ? minLog2TileCols : t;
        h->uniform_tile_spacing = (int)f1(&r);
        if (h->uniform_tile_spacing) {
            h->tile_cols_log2 = minLog2TileCols;
            while (h->tile_cols_log2 < maxLog2TileCols && f1(&r)) h->tile_cols_log2++;
            const int tw = (sbCols + (1 << h->tile_cols_log2) - 1) >> h->tile_cols_log2;
            int i = 0;
            for (int sb = 0; sb < sbCols && i < 64; sb += tw) h->mi_col_starts[i++] = sb << sbShift;
            h->mi_col_starts[i] = h->mi_cols; h->tile_cols = i;
            const int minLog2TileRows = minLog2Tiles - h->tile_cols_log2 > 0 ? minLog2Tiles - h->tile_cols_log2 : 0;
            h->tile_rows_log2 = minLog2TileRows;
            while (h->tile_rows_log2 < maxLog2TileRows && f1(&r)) h->tile_rows_log2++;
            const int th = (sbRows + (1 << h->tile_rows_log2) - 1) >> h->tile_rows_log2;
            i = 0;
            for (int sb = 0; sb < sbRows && i < 64; sb += th) h->mi_row_starts[i++] = sb << sbShift;
            h->mi_row_starts[i] = h->mi_rows; h->tile_rows = i;
        } else {
            int widest = 0, sb = 0, i;
            for (i = 0; sb < sbCols && i < 64 && !r.err; i++) {
                h->mi_col_starts[i] = sb << sbShift;
                const int maxW = sbCols - sb < maxTileWidthSb ? sbCols - sb : maxTileWidthSb;
                const int size = ns(&r, maxW) + 1;
                if (size > widest) widest = size;
                sb += size;
            }
            h->mi_col_starts[i] = h->mi_cols; h->tile_cols = i; h->tile_cols_log2 = tile_log2(1, h->tile_cols);
            maxTileAreaSb = minLog2Tiles > 0 ? (sbRows * sbCols) >> (minLog2Tiles + 1) : sbRows * sbCols;
            const int maxTileHeightSb = widest ? (maxTileAreaSb / widest > 1 ? maxTileAreaSb / widest : 1) : 1;
            sb = 0;
            for (i = 0; sb < sbRows && i < 64 && !r.err; i++) {
                h->mi_row_starts[i] = sb << sbShift;
                const int maxH = sbRows - sb < maxTileHeightSb ? sbRows - sb : maxTileHeightSb;
                sb += ns(&r, maxH) + 1;
            }
            h->mi_row_starts[i] = h->mi_rows; h->tile_rows = i; h->tile_rows_log2 = tile_log2(1, h->tile_rows);
        }
        if (h->tile_cols_log2 > 0 || h->tile_rows_log2 > 0) {
            h->context_update_tile_id = (int)f(&r, h->tile_rows_log2 + h->tile_cols_log2);
            h->tile_size_bytes = (int)f(&r, 2) + 1;
        } else { h->context_update_tile_id = 0; h->tile_size_bytes = 4; }
    }
    // quantization (5.9.12)
    h->base_q_idx = (int)f(&r, 8);
    h->delta_q_y_dc = read_delta_q(&r);
    if (!s->mono_chrome) {
        h->diff_uv_delta = s->separate_uv_delta_q ? (int)f1(&r) : 0;
        h->delta_q_u_dc = read_delta_q(&r); h->delta_q_u_ac = read_delta_q(&r);
        if (h->diff_uv_delta) { h->delta_q_v_dc = read_delta_q(&r); h->delta_q_v_ac = read_delta_q(&r); }
        else { h->delta_q_v_dc = h->delta_q_u_dc; h->delta_q_v_ac = h->delta_q_u_ac; }
    }
    h->using_qmatrix = (int)f1(&r);
    if (h->using_qmatrix) {
        h->qm_y = (int)f(&r, 4); h->qm_u = (int)f(&r, 4);
        h->qm_v = s->separate_uv_delta_q ? (int)f(&r, 4) : h->qm_u;
    }
    // segmentation (5.9.14)
    h->seg_enabled = (int)f1(&r);
    if (h->seg_enabled) {
        if (h->primary_ref_frame == AV1_PRIMARY_REF_NONE) { h->seg_update_map = 1; h->seg_temporal_update = 0; h->seg_update_data = 1; }
        else {
            h->seg_update_map = (int)f1(&r);
            if (h->seg_update_map) h->seg_temporal_update = (int)f1(&r);
            h->seg_update_data = (int)f1(&r);
        }
        if (h->seg_update_data) {
            static const int bits[8] = { 8, 6, 6, 6, 6, 3, 0, 0 }, sgn[8] = { 1, 1, 1, 1, 1, 0, 0, 0 }, mx[8] = { 255, 63, 63, 63, 63, 7, 0, 0 };
            for (int i = 0; i < MAX_SEGMENTS; i++) for (int j = 0; j < SEG_LVL_MAX; j++) {
                int v = 0;
                h->seg_feature_enabled[i][j] = (int)f1(&r);
                if (h->seg_feature_enabled[i][j]) {
                    if (sgn[j]) { v = su(&r, 1 + bits[j]); v = v < -mx[j] ? -mx[j] : v > mx[j] ? mx[j] : v; }
                    else { v = (int)f(&r, bits[j]); v = v > mx[j] ? mx[j] : v; }
                }
                h->seg_feature_data[i][j] = v;
            }
        }
    } else { memset(h->seg_feature_enabled, 0, sizeof h->seg_feature_enabled); memset(h->seg_feature_data, 0, sizeof h->seg_feature_data); }
    if (h->primary_ref_frame == AV1_PRIMARY_REF_NONE && !h->seg_enabled) { /* setup_past_independence cleared them too */ }
    for (int i = 0; i < MAX_SEGMENTS; i++) for (int j = 0; j < SEG_LVL_MAX; j++)
        if (h->seg_feature_enabled[i][j]) { h->last_active_seg_id = i; if (j >= SEG_LVL_REF_FRAME) h->seg_id_pre_skip = 1; }
    // delta q / lf (5.9.17-18)
    if (h->base_q_idx > 0) h->delta_q_present = (int)f1(&r);
    if (h->delta_q_present) h->delta_q_res = (int)f(&r, 2);
    if (h->delta_q_present) {
        if (!h->allow_intrabc) h->delta_lf_present = (int)f1(&r);
        if (h->delta_lf_present) { h->delta_lf_res = (int)f(&r, 2); h->delta_lf_multi = (int)f1(&r); }
    }
    h->coded_lossless = 1;
    for (int seg = 0; seg < MAX_SEGMENTS; seg++) {
        h->lossless[seg] = get_qindex(h, seg) == 0 && h->delta_q_y_dc == 0 && h->delta_q_u_ac == 0 && h->delta_q_u_dc == 0 &&
                           h->delta_q_v_ac == 0 && h->delta_q_v_dc == 0;
        if (!h->lossless[seg]) h->coded_lossless = 0;
    }
    h->all_lossless = h->coded_lossless && h->frame_width == h->upscaled_width;
    // loop filter (5.9.11)
    if (h->coded_lossless || h->allow_intrabc) {
        static const int d[8] = { 1, 0, 0, 0, -1, 0, -1, -1 };
        memcpy(h->lf_ref_deltas, d, sizeof d); h->lf_mode_deltas[0] = h->lf_mode_deltas[1] = 0;
    } else {
        h->lf_level[0] = (int)f(&r, 6); h->lf_level[1] = (int)f(&r, 6);
        if (!s->mono_chrome && (h->lf_level[0] || h->lf_level[1])) { h->lf_level[2] = (int)f(&r, 6); h->lf_level[3] = (int)f(&r, 6); }
        h->lf_sharpness = (int)f(&r, 3);
        h->lf_delta_enabled = (int)f1(&r);
        if (h->lf_delta_enabled) {
            h->lf_delta_update = (int)f1(&r);
            if (h->lf_delta_update) {
                for (int i = 0; i < 8; i++) if (f1(&r)) h->lf_ref_deltas[i] = su(&r, 7);
                for (int i = 0; i < 2; i++) if (f1(&r)) h->lf_mode_deltas[i] = su(&r, 7);
            }
        }
    }
    // CDEF (5.9.19)
    if (h->coded_lossless || h->allow_intrabc || !s->enable_cdef) h->cdef_damping = 3;
    else {
        h->cdef_damping = (int)f(&r, 2) + 3; h->cdef_bits = (int)f(&r, 2);
        for (int i = 0; i < (1 << h->cdef_bits); i++) {
            h->cdef_y_pri[i] = (int)f(&r, 4); h->cdef_y_sec[i] = (int)f(&r, 2); if (h->cdef_y_sec[i] == 3) h->cdef_y_sec[i]++;
            if (!s->mono_chrome) { h->cdef_uv_pri[i] = (int)f(&r, 4); h->cdef_uv_sec[i] = (int)f(&r, 2); if (h->cdef_uv_sec[i] == 3) h->cdef_uv_sec[i]++; }
        }
    }
    // loop restoration (5.9.20)
    if (!(h->all_lossless || h->allow_intrabc || !s->enable_restoration)) {
        static const int remap[4] = { AV1_RESTORE_NONE, AV1_RESTORE_SWITCHABLE, AV1_RESTORE_WIENER, AV1_RESTORE_SGRPROJ };
        int chroma = 0;
        for (int i = 0; i < (s->mono_chrome ? 1 : 3); i++) {
            h->lr_type[i] = remap[f(&r, 2)];
            if (h->lr_type[i] != AV1_RESTORE_NONE) { h->uses_lr = 1; if (i > 0) chroma = 1; }
        }
        if (h->uses_lr) {
            if (s->use_128x128_superblock) h->lr_unit_shift = (int)f1(&r) + 1;
            else { h->lr_unit_shift = (int)f1(&r); if (h->lr_unit_shift) h->lr_unit_shift += (int)f1(&r); }
            h->lr_size[0] = 256 >> (2 - h->lr_unit_shift);
            h->lr_uv_shift = (s->subsampling_x && s->subsampling_y && chroma) ? (int)f1(&r) : 0;
            h->lr_size[1] = h->lr_size[2] = h->lr_size[0] >> h->lr_uv_shift;
        }
    }
    // tx mode, reference mode, skip mode (5.9.21-23)
    h->tx_mode = h->coded_lossless ? AV1_ONLY_4X4 : f1(&r) ? AV1_TX_MODE_SELECT : AV1_TX_MODE_LARGEST;
    h->reference_select = h->frame_is_intra ? 0 : (int)f1(&r);
    {
        int allowed = 0;
        if (!(h->frame_is_intra || !h->reference_select || !s->enable_order_hint)) {
            int fwd = -1, bwd = -1, fwdHint = 0, bwdHint = 0;
            for (int i = 0; i < 7; i++) {
                const int rh = st->ref[h->ref_frame_idx[i]].order_hint;
                if (av1_get_relative_dist(s, rh, h->order_hint) < 0) {
                    if (fwd < 0 || av1_get_relative_dist(s, rh, fwdHint) > 0) { fwd = i; fwdHint = rh; }
                } else if (av1_get_relative_dist(s, rh, h->order_hint) > 0) {
                    if (bwd < 0 || av1_get_relative_dist(s, rh, bwdHint) < 0) { bwd = i; bwdHint = rh; }
                }
            }
            if (fwd < 0) allowed = 0;
            else if (bwd >= 0) { allowed = 1; h->skip_mode_frame[0] = LAST_FRAME + (fwd < bwd ? fwd : bwd); h->skip_mode_frame[1] = LAST_FRAME + (fwd > bwd ? fwd : bwd); }
            else {
                int fwd2 = -1, fwd2Hint = 0;
                for (int i = 0; i < 7; i++) {
                    const int rh = st->ref[h->ref_frame_idx[i]].order_hint;
                    if (av1_get_relative_dist(s, rh, fwdHint) < 0 && (fwd2 < 0 || av1_get_relative_dist(s, rh, fwd2Hint) > 0)) { fwd2 = i; fwd2Hint = rh; }
                }
                if (fwd2 >= 0) { allowed = 1; h->skip_mode_frame[0] = LAST_FRAME + (fwd < fwd2 ? fwd : fwd2); h->skip_mode_frame[1] = LAST_FRAME + (fwd > fwd2 ? fwd : fwd2); }
            }
        }
        h->skip_mode_present = allowed ? (int)f1(&r) : 0;
    }
    h->allow_warped_motion = (h->frame_is_intra || h->error_resilient_mode || !s->enable_warped_motion) ? 0 : (int)f1(&r);
    h->reduced_tx_set = (int)f1(&r);
    // global motion (5.9.24)
    for (int rf = LAST_FRAME; rf <= ALTREF_FRAME; rf++) {
        h->gm_type[rf] = AV1_IDENTITY;
        for (int i = 0; i < 6; i++) h->gm_params[rf][i] = (i % 3 == 2) ? 1 << WARPEDMODEL_PREC_BITS : 0;
    }
    if (!h->frame_is_intra)
        for (int rf = LAST_FRAME; rf <= ALTREF_FRAME; rf++) {
            int type = AV1_IDENTITY;
            if (f1(&r)) { if (f1(&r)) type = AV1_ROTZOOM; else type = f1(&r) ? AV1_TRANSLATION : AV1_AFFINE; }
            h->gm_type[rf] = type;
            if (type >= AV1_ROTZOOM) {
                read_global_param(&r, h, (const int (*)[6])prevGm, type, rf, 2); read_global_param(&r, h, (const int (*)[6])prevGm, type, rf, 3);
                if (type == AV1_AFFINE) { read_global_param(&r, h, (const int (*)[6])prevGm, type, rf, 4); read_global_param(&r, h, (const int (*)[6])prevGm, type, rf, 5); }
                else { h->gm_params[rf][4] = -h->gm_params[rf][3]; h->gm_params[rf][5] = h->gm_params[rf][2]; }
            }
            if (type >= AV1_TRANSLATION) { read_global_param(&r, h, (const int (*)[6])prevGm, type, rf, 0); read_global_param(&r, h, (const int (*)[6])prevGm, type, rf, 1); }
        }
    film_grain_params(&r, s, st, h);
    byte_align(&r);
    *bytes = r.bit >> 3;
    return r.err ? -1 : 0;
}

int av1_parse_tile_group(const Av1Frame *h, const uint8_t *p, size_t n, Av1Tile *tiles, int max, int *tg_start, bool *last)
{
    Br r = { p, n, 0, false };
    const int numTiles = h->tile_cols * h->tile_rows;
    int start = 0, end = numTiles - 1;
    if (numTiles > 1 && f1(&r)) {
        const int bits = h->tile_cols_log2 + h->tile_rows_log2;
        start = (int)f(&r, bits); end = (int)f(&r, bits);
    }
    byte_align(&r);
    if (r.err || end >= numTiles || start > end) return -1;
    size_t pos = r.bit >> 3; int k = 0;
    for (int t = start; t <= end; t++) {
        size_t size;
        if (t == end) size = n - pos;
        else {
            if (pos + (size_t)h->tile_size_bytes > n) return -1;
            size = 0;
            for (int i = 0; i < h->tile_size_bytes; i++) size |= (size_t)p[pos + i] << (8 * i);
            size += 1; pos += (size_t)h->tile_size_bytes;
        }
        if (pos + size > n || k >= max) return -1;
        tiles[k++] = (Av1Tile){ pos, size, t / h->tile_cols, t % h->tile_cols };
        pos += size;
    }
    *tg_start = start; *last = end == numTiles - 1;
    return k;
}

void av1_end_frame(Av1State *st, Av1Frame *h)
{
    if (h->show_existing_frame) {
        if (h->frame_type != AV1_KEY_FRAME) return;
        // a shown key frame: load it (7.21) and store it into every slot (7.20)
        const Av1Ref src = st->ref[h->frame_to_show_map_idx];
        for (int i = 0; i < 8; i++) { st->ref[i] = src; st->ref[i].valid = 1; }
        return;
    }
    for (int i = 0; i < 8; i++) {
        if (!(h->refresh_frame_flags & (1 << i))) continue;
        Av1Ref *rf = &st->ref[i];
        rf->valid = 1; rf->frame_id = h->current_frame_id; rf->upscaled_width = h->upscaled_width;
        rf->frame_width = h->frame_width; rf->frame_height = h->frame_height; rf->render_width = h->render_width;
        rf->render_height = h->render_height; rf->mi_cols = h->mi_cols; rf->mi_rows = h->mi_rows; rf->frame_type = h->frame_type;
        rf->subsampling_x = st->seq.subsampling_x; rf->subsampling_y = st->seq.subsampling_y; rf->bit_depth = st->seq.bit_depth;
        rf->showable = h->showable_frame;
        memcpy(rf->saved_order_hints, h->order_hints, sizeof rf->saved_order_hints);
        memcpy(rf->gm_params, h->gm_params, sizeof rf->gm_params);
        memcpy(rf->lf_ref_deltas, h->lf_ref_deltas, sizeof rf->lf_ref_deltas); memcpy(rf->lf_mode_deltas, h->lf_mode_deltas, sizeof rf->lf_mode_deltas);
        memcpy(rf->seg_feature_enabled, h->seg_feature_enabled, sizeof rf->seg_feature_enabled);
        memcpy(rf->seg_feature_data, h->seg_feature_data, sizeof rf->seg_feature_data);
        rf->fg = h->fg;
        rf->order_hint = h->order_hint;
    }
}
