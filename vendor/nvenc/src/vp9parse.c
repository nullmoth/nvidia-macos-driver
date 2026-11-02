// vp9parse.c (M10): VP9 uncompressed / compressed headers, probability contexts and backward adaptation (see vp9parse.h).
#include "vp9parse.h"
#include "vp9tables.h"
#include "nvdec_drv.h"
#include <string.h>

// ---- bit reader (f(n), s(n)) ----
typedef struct { const uint8_t *b; size_t n, bit; bool err; } Br;
static unsigned f1(Br *r) { if (r->bit >= r->n * 8) { r->err = true; return 0; } unsigned v = r->b[r->bit >> 3] >> (7 - (r->bit & 7)) & 1; r->bit++; return v; }
static unsigned f(Br *r, int n) { unsigned v = 0; while (n-- > 0) v = v << 1 | f1(r); return v; }
static int s(Br *r, int n) { int v = (int)f(r, n); return f1(r) ? -v : v; }

// ---- boolean decoder (9.2) ----
typedef struct { Br br; unsigned value, range; long maxbits; } Bd;
static int bool_init(Bd *d, const uint8_t *b, size_t n)
{
    if (n < 1) return -1;
    d->br = (Br){ b, n, 0, false };
    d->value = f(&d->br, 8); d->range = 255; d->maxbits = 8 * (long)n - 8;
    return 0;
}
static int B(Bd *d, int p)
{
    const unsigned split = 1 + (((d->range - 1) * (unsigned)p) >> 8);
    int bit;
    if (d->value < split) { d->range = split; bit = 0; }
    else { d->range -= split; d->value -= split; bit = 1; }
    while (d->range < 128) {
        unsigned nb = 0;
        if (d->maxbits > 0) { nb = f1(&d->br); d->maxbits--; }
        d->range <<= 1; d->value = (d->value << 1) + nb;
    }
    return bit;
}
static unsigned L(Bd *d, int n) { unsigned x = 0; while (n-- > 0) x = 2 * x + (unsigned)B(d, 128); return x; }

// ---- defaults and contexts ----
static void default_probs(Vp9Probs *p)
{
    for (int i = 0; i < 2; i++) {
        p->tx8[i][0] = default_tx_probs[1][i][0];
        for (int j = 0; j < 2; j++) p->tx16[i][j] = default_tx_probs[2][i][j];
        for (int j = 0; j < 3; j++) p->tx32[i][j] = default_tx_probs[3][i][j];
    }
    memcpy(p->coef, default_coef_probs, sizeof p->coef);
    memcpy(p->skip, default_skip_prob, sizeof p->skip);
    memcpy(p->inter_mode, default_inter_mode_probs, sizeof p->inter_mode);
    memcpy(p->interp_filter, default_interp_filter_probs, sizeof p->interp_filter);
    memcpy(p->is_inter, default_is_inter_prob, sizeof p->is_inter);
    memcpy(p->comp_mode, default_comp_mode_prob, sizeof p->comp_mode);
    memcpy(p->single_ref, default_single_ref_prob, sizeof p->single_ref);
    memcpy(p->comp_ref, default_comp_ref_prob, sizeof p->comp_ref);
    memcpy(p->y_mode, default_y_mode_probs, sizeof p->y_mode);
    memcpy(p->uv_mode, default_uv_mode_probs, sizeof p->uv_mode);
    memcpy(p->partition, default_partition_probs, sizeof p->partition);
    memcpy(p->mv_joint, default_mv_joint_probs, sizeof p->mv_joint);
    memcpy(p->mv_sign, default_mv_sign_prob, sizeof p->mv_sign);
    memcpy(p->mv_class, default_mv_class_probs, sizeof p->mv_class);
    memcpy(p->mv_class0_bit, default_mv_class0_bit_prob, sizeof p->mv_class0_bit);
    memcpy(p->mv_bits, default_mv_bits_prob, sizeof p->mv_bits);
    memcpy(p->mv_class0_fr, default_mv_class0_fr_probs, sizeof p->mv_class0_fr);
    memcpy(p->mv_fr, default_mv_fr_probs, sizeof p->mv_fr);
    memcpy(p->mv_class0_hp, default_mv_class0_hp_prob, sizeof p->mv_class0_hp);
    memcpy(p->mv_hp, default_mv_hp_prob, sizeof p->mv_hp);
}

void vp9_init(Vp9State *st)
{
    memset(st, 0, sizeof *st);
    default_probs(&st->fc);
    for (int i = 0; i < 4; i++) st->ctx[i] = st->fc;
    st->frame_type = VP9_KEY_FRAME;
}

static void setup_past_independence(Vp9State *st)
{
    memset(st->seg_feature_enabled, 0, sizeof st->seg_feature_enabled);
    memset(st->seg_feature_data, 0, sizeof st->seg_feature_data);
    st->seg_abs_or_delta_update = 0;
    st->lf_delta_enabled = 1;
    st->lf_ref_deltas[0] = 1; st->lf_ref_deltas[1] = 0; st->lf_ref_deltas[2] = -1; st->lf_ref_deltas[3] = -1;
    st->lf_mode_deltas[0] = st->lf_mode_deltas[1] = 0;
    default_probs(&st->fc);
}

// ---- Annex B ----
int vp9_superframe(const uint8_t *buf, size_t n, size_t off[8], size_t len[8])
{
    if (n > 0) {
        const uint8_t last = buf[n - 1];
        if ((last & 0xe0) == 0xc0) {
            const int szb = ((last >> 3) & 3) + 1, nf = (last & 7) + 1;
            const size_t idx = 2 + (size_t)nf * (size_t)szb;
            if (n >= idx && buf[n - idx] == last) {
                const uint8_t *p = buf + n - idx + 1; size_t o = 0;
                for (int i = 0; i < nf; i++) {
                    size_t sz = 0;
                    for (int j = 0; j < szb; j++) sz |= (size_t)*p++ << (8 * j);   // little-endian
                    if (o + sz > n - idx) return -1;
                    off[i] = o; len[i] = sz; o += sz;
                }
                return nf;
            }
        }
    }
    off[0] = 0; len[0] = n;
    return 1;
}

// ---- uncompressed header (6.2) ----
static void color_config(Br *r, Vp9State *st, Vp9Frame *h)
{
    h->bit_depth = h->profile >= 2 ? (f1(r) ? 12 : 10) : 8;
    h->color_space = (int)f(r, 3);
    if (h->color_space != VP9_CS_RGB) {
        h->color_range = (int)f1(r);
        if (h->profile == 1 || h->profile == 3) { h->subsampling_x = (int)f1(r); h->subsampling_y = (int)f1(r); f1(r); }
        else h->subsampling_x = h->subsampling_y = 1;
    } else {
        h->color_range = 1;
        if (h->profile == 1 || h->profile == 3) { h->subsampling_x = h->subsampling_y = 0; f1(r); }
    }
    st->bit_depth = h->bit_depth; st->color_space = h->color_space; st->color_range = h->color_range;
    st->subsampling_x = h->subsampling_x; st->subsampling_y = h->subsampling_y;
}

static void frame_size(Br *r, Vp9Frame *h) { h->width = (int)f(r, 16) + 1; h->height = (int)f(r, 16) + 1; }

static void render_size(Br *r, Vp9Frame *h)
{
    if (f1(r)) { h->render_width = (int)f(r, 16) + 1; h->render_height = (int)f(r, 16) + 1; }
    else { h->render_width = h->width; h->render_height = h->height; }
}

static int sync_code(Br *r) { return f(r, 8) == 0x49 && f(r, 8) == 0x83 && f(r, 8) == 0x42 ? 0 : -1; }

static uint8_t read_prob(Br *r) { return f1(r) ? (uint8_t)f(r, 8) : 255; }

static int read_delta_q(Br *r) { return f1(r) ? s(r, 4) : 0; }

// ---- compressed header (6.3) ----
static const uint8_t inv_map_table[255] = {
    7, 20, 33, 46, 59, 72, 85, 98, 111, 124, 137, 150, 163, 176, 189, 202, 215, 228, 241, 254, 1, 2, 3, 4, 5, 6, 8, 9,
    10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 34, 35, 36, 37, 38, 39, 40,
    41, 42, 43, 44, 45, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71,
    73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 99, 100, 101, 102,
    103, 104, 105, 106, 107, 108, 109, 110, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 125, 126, 127,
    128, 129, 130, 131, 132, 133, 134, 135, 136, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 151, 152,
    153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 177,
    178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 190, 191, 192, 193, 194, 195, 196, 197, 198, 199, 200, 201,
    203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226,
    227, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238, 239, 240, 242, 243, 244, 245, 246, 247, 248, 249, 250, 251,
    252, 253, 253,
};

static int inv_recenter_nonneg(int v, int m)
{
    if (v > 2 * m) return v;
    return (v & 1) ? m - ((v + 1) >> 1) : m + (v >> 1);
}

static int decode_term_subexp(Bd *d)
{
    if (!L(d, 1)) return (int)L(d, 4);
    if (!L(d, 1)) return (int)L(d, 4) + 16;
    if (!L(d, 1)) return (int)L(d, 5) + 32;
    const int v = (int)L(d, 7);
    if (v < 65) return v + 64;
    return (v << 1) - 1 + (int)L(d, 1);
}

static int inv_remap_prob(int delta, int prob)
{
    const int v = inv_map_table[delta < 255 ? delta : 254];
    int m = prob - 1;
    if ((m << 1) <= 255) return 1 + inv_recenter_nonneg(v, m);
    return 255 - inv_recenter_nonneg(v, 255 - 1 - m);
}

static void diff_update(Bd *d, uint8_t *p)
{
    if (B(d, 252)) *p = (uint8_t)inv_remap_prob(decode_term_subexp(d), *p);
}

static void mv_update(Bd *d, uint8_t *p)
{
    if (B(d, 252)) *p = (uint8_t)((L(d, 7) << 1) | 1);
}

static void compressed_header(Bd *d, Vp9State *st, Vp9Frame *h)
{
    Vp9Probs *p = &st->fc;
    // tx mode
    if (h->lossless) h->tx_mode = VP9_ONLY_4X4;
    else { h->tx_mode = (int)L(d, 2); if (h->tx_mode == VP9_ALLOW_32X32) h->tx_mode += (int)L(d, 1); }
    if (h->tx_mode == VP9_TX_MODE_SELECT) {
        for (int i = 0; i < 2; i++) diff_update(d, &p->tx8[i][0]);
        for (int i = 0; i < 2; i++) for (int j = 0; j < 2; j++) diff_update(d, &p->tx16[i][j]);
        for (int i = 0; i < 2; i++) for (int j = 0; j < 3; j++) diff_update(d, &p->tx32[i][j]);
    }
    // coefficient probabilities
    static const int biggest_tx[5] = { 0, 1, 2, 3, 3 };
    for (int t = 0; t <= biggest_tx[h->tx_mode]; t++) {
        if (!L(d, 1)) continue;
        for (int i = 0; i < 2; i++) for (int j = 0; j < 2; j++) for (int k = 0; k < 6; k++)
            for (int l = 0; l < (k == 0 ? 3 : 6); l++) for (int m = 0; m < 3; m++) diff_update(d, &p->coef[t][i][j][k][l][m]);
    }
    for (int i = 0; i < 3; i++) diff_update(d, &p->skip[i]);
    if (h->frame_is_intra) return;
    for (int i = 0; i < 7; i++) for (int j = 0; j < 3; j++) diff_update(d, &p->inter_mode[i][j]);
    if (h->interp_filter == VP9_SWITCHABLE)
        for (int j = 0; j < 4; j++) for (int i = 0; i < 2; i++) diff_update(d, &p->interp_filter[j][i]);
    for (int i = 0; i < 4; i++) diff_update(d, &p->is_inter[i]);
    // frame reference mode
    int compound_allowed = 0;
    for (int i = 1; i < 3; i++) if (h->ref_frame_sign_bias[i + 1] != h->ref_frame_sign_bias[1]) compound_allowed = 1;
    h->reference_mode = VP9_SINGLE_REFERENCE;
    if (compound_allowed && L(d, 1)) {
        h->reference_mode = L(d, 1) ? VP9_REFERENCE_MODE_SELECT : VP9_COMPOUND_REFERENCE;
        const int *sb = h->ref_frame_sign_bias;
        if (sb[VP9_LAST_FRAME] == sb[VP9_GOLDEN_FRAME]) {
            h->comp_fixed_ref = VP9_ALTREF_FRAME; h->comp_var_ref[0] = VP9_LAST_FRAME; h->comp_var_ref[1] = VP9_GOLDEN_FRAME;
        } else if (sb[VP9_LAST_FRAME] == sb[VP9_ALTREF_FRAME]) {
            h->comp_fixed_ref = VP9_GOLDEN_FRAME; h->comp_var_ref[0] = VP9_LAST_FRAME; h->comp_var_ref[1] = VP9_ALTREF_FRAME;
        } else {
            h->comp_fixed_ref = VP9_LAST_FRAME; h->comp_var_ref[0] = VP9_GOLDEN_FRAME; h->comp_var_ref[1] = VP9_ALTREF_FRAME;
        }
    }
    if (h->reference_mode == VP9_REFERENCE_MODE_SELECT) for (int i = 0; i < 5; i++) diff_update(d, &p->comp_mode[i]);
    if (h->reference_mode != VP9_COMPOUND_REFERENCE)
        for (int i = 0; i < 5; i++) { diff_update(d, &p->single_ref[i][0]); diff_update(d, &p->single_ref[i][1]); }
    if (h->reference_mode != VP9_SINGLE_REFERENCE) for (int i = 0; i < 5; i++) diff_update(d, &p->comp_ref[i]);
    for (int i = 0; i < 4; i++) for (int j = 0; j < 9; j++) diff_update(d, &p->y_mode[i][j]);
    for (int i = 0; i < 16; i++) for (int j = 0; j < 3; j++) diff_update(d, &p->partition[i][j]);
    // motion vector probabilities
    for (int j = 0; j < 3; j++) mv_update(d, &p->mv_joint[j]);
    for (int i = 0; i < 2; i++) {
        mv_update(d, &p->mv_sign[i]);
        for (int j = 0; j < 10; j++) mv_update(d, &p->mv_class[i][j]);
        mv_update(d, &p->mv_class0_bit[i]);
        for (int j = 0; j < 10; j++) mv_update(d, &p->mv_bits[i][j]);
    }
    for (int i = 0; i < 2; i++) {
        for (int j = 0; j < 2; j++) for (int k = 0; k < 3; k++) mv_update(d, &p->mv_class0_fr[i][j][k]);
        for (int k = 0; k < 3; k++) mv_update(d, &p->mv_fr[i][k]);
    }
    if (h->allow_high_precision_mv)
        for (int i = 0; i < 2; i++) { mv_update(d, &p->mv_class0_hp[i]); mv_update(d, &p->mv_hp[i]); }
}

int vp9_parse_frame(Vp9State *st, const uint8_t *buf, size_t n, Vp9Frame *h)
{
    Br r = { buf, n, 0, false };
    memset(h, 0, sizeof *h);
    if (f(&r, 2) != 2) return -1;
    h->profile = (int)f1(&r); h->profile |= (int)f1(&r) << 1;
    if (h->profile == 3) f1(&r);
    h->show_existing_frame = (int)f1(&r);
    if (h->show_existing_frame) { h->frame_to_show_map_idx = (int)f(&r, 3); h->uncomp_bytes = (int)((r.bit + 7) >> 3); return r.err ? -1 : 0; }
    h->last_frame_type = st->frame_type;
    h->frame_type = (int)f1(&r);
    h->show_frame = (int)f1(&r);
    h->error_resilient_mode = (int)f1(&r);
    // what inter frames inherit
    h->bit_depth = st->bit_depth; h->color_space = st->color_space; h->color_range = st->color_range;
    h->subsampling_x = st->subsampling_x; h->subsampling_y = st->subsampling_y;
    if (h->frame_type == VP9_KEY_FRAME) {
        if (sync_code(&r)) return -1;
        color_config(&r, st, h);
        frame_size(&r, h); render_size(&r, h);
        h->refresh_frame_flags = 0xff;
        h->frame_is_intra = 1;
    } else {
        h->intra_only = h->show_frame ? 0 : (int)f1(&r);
        if (!st->started && !h->intra_only) return -1;      // a stream starts with a key frame or an intra-only frame (8.2)
        h->frame_is_intra = h->intra_only;
        h->reset_frame_context = h->error_resilient_mode ? 0 : (int)f(&r, 2);
        if (h->intra_only) {
            if (sync_code(&r)) return -1;
            if (h->profile > 0) color_config(&r, st, h);
            else {
                h->color_space = 2; h->subsampling_x = h->subsampling_y = 1; h->bit_depth = 8;   // CS_BT_601
                st->color_space = 2; st->subsampling_x = st->subsampling_y = 1; st->bit_depth = 8;
            }
            h->refresh_frame_flags = (int)f(&r, 8);
            frame_size(&r, h); render_size(&r, h);
        } else {
            h->refresh_frame_flags = (int)f(&r, 8);
            for (int i = 0; i < 3; i++) { h->ref_frame_idx[i] = (int)f(&r, 3); h->ref_frame_sign_bias[VP9_LAST_FRAME + i] = (int)f1(&r); }
            int found = 0;
            for (int i = 0; i < 3 && !found; i++)
                if (f1(&r)) { found = 1; h->width = st->ref_width[h->ref_frame_idx[i]]; h->height = st->ref_height[h->ref_frame_idx[i]]; }
            if (!found) frame_size(&r, h);
            h->size_from_ref = found;
            render_size(&r, h);
            h->allow_high_precision_mv = (int)f1(&r);
            if (f1(&r)) h->interp_filter = VP9_SWITCHABLE;
            else {
                static const int literal_to_type[4] = { VP9_EIGHTTAP_SMOOTH, VP9_EIGHTTAP, VP9_EIGHTTAP_SHARP, VP9_BILINEAR };
                h->interp_filter = literal_to_type[f(&r, 2)];
            }
        }
    }
    if (!h->error_resilient_mode) { h->refresh_frame_context = (int)f1(&r); h->frame_parallel_decoding_mode = (int)f1(&r); }
    else { h->refresh_frame_context = 0; h->frame_parallel_decoding_mode = 1; }
    h->frame_context_idx = (int)f(&r, 2);
    if (h->frame_is_intra || h->error_resilient_mode) {
        setup_past_independence(st);
        if (h->frame_type == VP9_KEY_FRAME || h->error_resilient_mode || h->reset_frame_context == 3)
            for (int i = 0; i < 4; i++) st->ctx[i] = st->fc;
        else if (h->reset_frame_context == 2) st->ctx[h->frame_context_idx] = st->fc;
        h->frame_context_idx = 0;
        memset(h->ref_frame_sign_bias, 0, sizeof h->ref_frame_sign_bias);   // also for error-resilient inter frames (7.2)
    }
    // loop filter
    h->lf_level = (int)f(&r, 6); h->lf_sharpness = (int)f(&r, 3);
    st->lf_delta_enabled = (int)f1(&r);
    if (st->lf_delta_enabled && f1(&r)) {
        for (int i = 0; i < 4; i++) if (f1(&r)) st->lf_ref_deltas[i] = s(&r, 6);
        for (int i = 0; i < 2; i++) if (f1(&r)) st->lf_mode_deltas[i] = s(&r, 6);
    }
    // quantization
    h->base_q_idx = (int)f(&r, 8);
    h->delta_q_y_dc = read_delta_q(&r); h->delta_q_uv_dc = read_delta_q(&r); h->delta_q_uv_ac = read_delta_q(&r);
    h->lossless = h->base_q_idx == 0 && h->delta_q_y_dc == 0 && h->delta_q_uv_dc == 0 && h->delta_q_uv_ac == 0;
    // segmentation
    h->seg_enabled = (int)f1(&r);
    if (h->seg_enabled) {
        h->seg_update_map = (int)f1(&r);
        if (h->seg_update_map) {
            for (int i = 0; i < 7; i++) h->seg_tree_probs[i] = read_prob(&r);
            h->seg_temporal_update = (int)f1(&r);
            for (int i = 0; i < 3; i++) h->seg_pred_probs[i] = h->seg_temporal_update ? read_prob(&r) : 255;
        }
        h->seg_update_data = (int)f1(&r);
        if (h->seg_update_data) {
            static const int bits[4] = { 8, 6, 2, 0 }, sgn[4] = { 1, 1, 0, 0 };
            st->seg_abs_or_delta_update = (int)f1(&r);
            for (int i = 0; i < 8; i++) for (int j = 0; j < 4; j++) {
                int v = 0;
                st->seg_feature_enabled[i][j] = (int)f1(&r);
                if (st->seg_feature_enabled[i][j]) { v = (int)f(&r, bits[j]); if (sgn[j] && f1(&r)) v = -v; }
                st->seg_feature_data[i][j] = v;
            }
        }
    }
    // tiles
    const int mi_cols = (h->width + 7) >> 3, sb64_cols = (mi_cols + 7) >> 3;
    int min_log2 = 0; while ((64 << min_log2) < sb64_cols) min_log2++;
    int max_log2 = 1; while ((sb64_cols >> max_log2) >= 4) max_log2++;
    max_log2--;
    h->tile_cols_log2 = min_log2;
    while (h->tile_cols_log2 < max_log2 && f1(&r)) h->tile_cols_log2++;
    h->tile_rows_log2 = (int)f1(&r);
    if (h->tile_rows_log2) h->tile_rows_log2 += (int)f1(&r);
    h->header_size_in_bytes = (int)f(&r, 16);
    h->uncomp_bytes = (int)((r.bit + 7) >> 3);
    if (r.err || h->header_size_in_bytes == 0 || (size_t)h->uncomp_bytes + (size_t)h->header_size_in_bytes > n) return -1;
    st->frame_type = h->frame_type;
    st->profile = h->profile;
    if (h->frame_is_intra) st->started = true;
    // load_probs + load_probs2, then the compressed header's updates
    st->fc = st->ctx[h->frame_context_idx];
    Bd d;
    if (bool_init(&d, buf + h->uncomp_bytes, (size_t)h->header_size_in_bytes) || B(&d, 128)) return -1;
    compressed_header(&d, st, h);
    h->pad_bits = (int)d.maxbits;
    for (long i = 0; i < d.maxbits; i++) h->pad_nonzero |= (int)f1(&d.br);
    st->interp_filter = h->interp_filter; st->allow_high_precision_mv = h->allow_high_precision_mv;
    return d.br.err ? -1 : 0;
}

// ---- backward adaptation (8.4) ----
static uint8_t merge_prob(int pre, unsigned ct0, unsigned ct1, unsigned sat, unsigned maxf)
{
    const unsigned den = ct0 + ct1;
    unsigned prob = 128;
    if (den) { unsigned long long q = ((unsigned long long)ct0 * 256 + (den >> 1)) / den; prob = q < 1 ? 1 : q > 255 ? 255 : (unsigned)q; }
    const unsigned count = den < sat ? den : sat, factor = maxf * count / sat;
    return (uint8_t)(((unsigned)pre * (256 - factor) + prob * factor + 128) >> 8);
}

static unsigned merge_probs(const int8_t *tree, int i, uint8_t *probs, const unsigned *counts, unsigned sat, unsigned maxf)
{
    const int sl = tree[i], sr = tree[i + 1];
    const unsigned l = sl <= 0 ? counts[-sl] : merge_probs(tree, sl, probs, counts, sat, maxf);
    const unsigned r = sr <= 0 ? counts[-sr] : merge_probs(tree, sr, probs, counts, sat, maxf);
    probs[i >> 1] = merge_prob(probs[i >> 1], l, r, sat, maxf);
    return l + r;
}

static void adapt_probs(const int8_t *tree, uint8_t *probs, const unsigned *counts) { merge_probs(tree, 0, probs, counts, 20, 128); }
static uint8_t adapt_prob(uint8_t p, const unsigned c[2]) { return merge_prob(p, c[0], c[1], 20, 128); }

static const int8_t intra_mode_tree[18] = { -0, 2, -9, 4, -1, 6, 8, 12, -2, 10, -4, -5, -3, 14, -8, 16, -6, -7 };
static const int8_t partition_tree[6] = { -0, 2, -1, 4, -2, -3 };
static const int8_t interp_filter_tree[4] = { -VP9_EIGHTTAP, 2, -VP9_EIGHTTAP_SMOOTH, -VP9_EIGHTTAP_SHARP };
static const int8_t tx_size_32_tree[6] = { -0, 2, -1, 4, -2, -3 };
static const int8_t tx_size_16_tree[4] = { -0, 2, -1, -2 };
static const int8_t tx_size_8_tree[2] = { -0, -1 };
static const int8_t inter_mode_tree[6] = { -2, 2, -0, 4, -1, -3 };   // values: NEARESTMV 0, NEARMV 1, ZEROMV 2, NEWMV 3
static const int8_t mv_joint_tree[6] = { -0, 2, -1, 4, -2, -3 };
static const int8_t mv_class_tree[20] = { -0, 2, -1, 4, 6, 8, -2, -3, 10, 12, -4, -5, -6, 14, 16, 18, -7, -8, -9, -10 };
static const int8_t mv_fr_tree[6] = { -0, 2, -1, 4, -2, -3 };
static const int8_t small_token_tree[6] = { 0, 0, -0, 4, -1, -2 };

static void adapt_coef(Vp9Probs *p, const nvdec_vp9EntropyCounts_t *c, unsigned uf)
{
    for (int t = 0; t < 4; t++) {
        const unsigned (*cc)[2][6][6][4] = t == 0 ? c->countCoeffs : t == 1 ? c->countCoeffs8x8 : t == 2 ? c->countCoeffs16x16 : c->countCoeffs32x32;
        for (int i = 0; i < 2; i++) for (int j = 0; j < 2; j++) for (int k = 0; k < 6; k++) for (int l = 0; l < (k == 0 ? 3 : 6); l++) {
            // NVDEC's counts: [0] ZERO_TOKEN, [1] ONE_TOKEN, [2] TWO_TOKEN or more, [3] no more coefficients; countEobs =
            // the number of times more_coefs was read
            const unsigned *n = cc[i][j][k][l];
            const unsigned tok[3] = { n[0], n[1], n[2] };
            const unsigned eob = c->countEobs[t][i][j][k][l];
            const unsigned more[2] = { n[3], eob > n[3] ? eob - n[3] : 0 };
            uint8_t *pr = p->coef[t][i][j][k][l];
            merge_probs(small_token_tree, 2, pr, tok, 24, uf);
            pr[0] = merge_prob(pr[0], more[0], more[1], 24, uf);
        }
    }
}

static void adapt_noncoef(Vp9Probs *p, const Vp9Frame *h, const nvdec_vp9EntropyCounts_t *c)
{
    for (int i = 0; i < 4; i++) p->is_inter[i] = adapt_prob(p->is_inter[i], c->intra_inter_count[i]);
    for (int i = 0; i < 5; i++) p->comp_mode[i] = adapt_prob(p->comp_mode[i], c->comp_inter_count[i]);
    for (int i = 0; i < 5; i++) p->comp_ref[i] = adapt_prob(p->comp_ref[i], c->comp_ref_count[i]);
    for (int i = 0; i < 5; i++) for (int j = 0; j < 2; j++) p->single_ref[i][j] = adapt_prob(p->single_ref[i][j], c->single_ref_count[i][j]);
    for (int i = 0; i < 7; i++) {
        // NVDEC counts the inter mode tree's branches: [0] ZEROMV / other, [1] NEARESTMV / other, [2] NEARMV / NEWMV
        const unsigned v[4] = { c->inter_mode_counts[i][1][0], c->inter_mode_counts[i][2][0], c->inter_mode_counts[i][0][0], c->inter_mode_counts[i][2][1] };
        adapt_probs(inter_mode_tree, p->inter_mode[i], v);
    }
    for (int i = 0; i < 4; i++) adapt_probs(intra_mode_tree, p->y_mode[i], c->sb_ymode_counts[i]);
    for (int i = 0; i < 10; i++) adapt_probs(intra_mode_tree, p->uv_mode[i], c->uv_mode_counts[i]);
    for (int i = 0; i < 16; i++) adapt_probs(partition_tree, p->partition[i], c->partition_counts[i]);
    for (int i = 0; i < 3; i++) p->skip[i] = adapt_prob(p->skip[i], c->mbskip_count[i]);
    if (h->interp_filter == VP9_SWITCHABLE)
        for (int i = 0; i < 4; i++) adapt_probs(interp_filter_tree, p->interp_filter[i], c->switchable_interp_counts[i]);
    if (h->tx_mode == VP9_TX_MODE_SELECT)
        for (int i = 0; i < 2; i++) {
            adapt_probs(tx_size_8_tree, p->tx8[i], c->tx8x8_count[i]);
            adapt_probs(tx_size_16_tree, p->tx16[i], c->tx16x16_count[i]);
            adapt_probs(tx_size_32_tree, p->tx32[i], c->tx32x32_count[i]);
        }
    const nvdec_nmv_context_counts *m = &c->nmvcount;
    adapt_probs(mv_joint_tree, p->mv_joint, m->joints);
    for (int i = 0; i < 2; i++) {
        p->mv_sign[i] = adapt_prob(p->mv_sign[i], m->sign[i]);
        adapt_probs(mv_class_tree, p->mv_class[i], m->classes[i]);
        p->mv_class0_bit[i] = adapt_prob(p->mv_class0_bit[i], m->class0[i]);
        for (int j = 0; j < 10; j++) p->mv_bits[i][j] = adapt_prob(p->mv_bits[i][j], m->bits[i][j]);
        for (int j = 0; j < 2; j++) adapt_probs(mv_fr_tree, p->mv_class0_fr[i][j], m->class0_fp[i][j]);
        adapt_probs(mv_fr_tree, p->mv_fr[i], m->fp[i]);
        if (h->allow_high_precision_mv) {
            p->mv_class0_hp[i] = adapt_prob(p->mv_class0_hp[i], m->class0_hp[i]);
            p->mv_hp[i] = adapt_prob(p->mv_hp[i], m->hp[i]);
        }
    }
}

void vp9_end_frame(Vp9State *st, const Vp9Frame *h, const struct nvdec_vp9EntropyCounts_s *counts)
{
    if (h->show_existing_frame) return;
    if (vp9_adapts(h) && counts) {
        // refresh_probs (6.1.2): load_probs, adapt the coefficients; for inter frames load_probs2 and adapt the rest
        const Vp9Probs *pre = &st->ctx[h->frame_context_idx];
        Vp9Probs a = *pre;
        memcpy(a.tx8, st->fc.tx8, sizeof a.tx8); memcpy(a.tx16, st->fc.tx16, sizeof a.tx16); memcpy(a.tx32, st->fc.tx32, sizeof a.tx32);
        memcpy(a.skip, st->fc.skip, sizeof a.skip);
        adapt_coef(&a, counts, h->frame_is_intra ? 112 : h->last_frame_type == VP9_KEY_FRAME ? 128 : 112);
        if (!h->frame_is_intra) {
            memcpy(a.tx8, pre->tx8, sizeof a.tx8); memcpy(a.tx16, pre->tx16, sizeof a.tx16); memcpy(a.tx32, pre->tx32, sizeof a.tx32);
            memcpy(a.skip, pre->skip, sizeof a.skip);
            adapt_noncoef(&a, h, counts);
        }
        st->fc = a;
    }
    if (h->refresh_frame_context) st->ctx[h->frame_context_idx] = st->fc;
    for (int i = 0; i < 8; i++)
        if (h->refresh_frame_flags & (1 << i)) { st->ref_width[i] = h->width; st->ref_height[i] = h->height; }
}
