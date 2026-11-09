// av1hdr.c: see av1hdr.h. Section numbers refer to the AV1 specification.
#include "av1hdr.h"
#include <string.h>

typedef struct { uint8_t *buf; size_t bits; } Bw;
static void put(Bw *b, unsigned v, int n) { for (int i = n - 1; i >= 0; i--, b->bits++) { if ((v >> i) & 1) b->buf[b->bits >> 3] |= (uint8_t)(0x80 >> (b->bits & 7)); else b->buf[b->bits >> 3] &= (uint8_t)~(0x80 >> (b->bits & 7)); } }
static void su(Bw *b, int v, int n) { put(b, (unsigned)v & ((1u << n) - 1), n); }   // 4.10.6
static void align(Bw *b) { while (b->bits & 7) put(b, 0, 1); }
static void trailing(Bw *b) { put(b, 1, 1); align(b); }                                // 5.3.4

static size_t leb128(size_t v, uint8_t *out)
{
    size_t n = 0;
    do { uint8_t x = v & 0x7f; v >>= 7; out[n++] = (uint8_t)(x | (v ? 0x80 : 0)); } while (v);
    return n;
}
// OBU header (5.3.2: no extension, size field present) + size + payload
static size_t obu(int type, const uint8_t *payload, size_t n, uint8_t *out)
{
    size_t o = 0; out[o++] = (uint8_t)(type << 3 | 2);
    o += leb128(n, out + o);
    memmove(out + o, payload, n);
    return o + n;
}
enum { OBU_SEQUENCE_HEADER = 1, OBU_TEMPORAL_DELIMITER = 2, OBU_FRAME_HEADER = 3, OBU_METADATA = 5, OBU_FRAME = 6 };

int av1_level_for(int width, int height)   // Annex A.3: MaxPicSize per level; the .1 levels for 60 fps
{
    long ps = (long)width * height;
    return ps <= 665856 ? 4 : ps <= 1065024 ? 5 : ps <= 2359296 ? 9 : ps <= 8912896 ? 13 : 17;   // 3.0, 3.1, 4.1, 5.1, 6.1
}

size_t av1_write_temporal_delimiter(uint8_t *out) { out[0] = OBU_TEMPORAL_DELIMITER << 3 | 2; out[1] = 0; return 2; }

static int bits_for(int v) { int n = 1; while ((1 << n) <= v) n++; return n; }

size_t av1_write_sequence_header(const Av1SeqParams *s, uint8_t *out)   // 5.5
{
    uint8_t p[64] = { 0 }; Bw b = { p, 0 };
    put(&b, 0, 3);                                             // seq_profile 0: 8 / 10-bit 4:2:0
    put(&b, 0, 1); put(&b, 0, 1);                              // still_picture, reduced_still_picture_header
    put(&b, 0, 1); put(&b, 0, 1);                              // timing_info_present_flag, initial_display_delay_present_flag
    put(&b, 0, 5); put(&b, 0, 12);                             // operating_points_cnt_minus_1, operating_point_idc[0]
    const int lvl = s->seq_level_idx ? s->seq_level_idx : av1_level_for(s->width, s->height);
    put(&b, (unsigned)lvl, 5); if (lvl > 7) put(&b, 0, 1);     // seq_level_idx, seq_tier (Main)
    const int wb = bits_for(s->width - 1), hb = bits_for(s->height - 1);
    put(&b, (unsigned)wb - 1, 4); put(&b, (unsigned)hb - 1, 4);
    put(&b, (unsigned)s->width - 1, wb); put(&b, (unsigned)s->height - 1, hb);
    put(&b, 0, 1);                                             // frame_id_numbers_present_flag
    put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1);               // use_128x128_superblock, enable_filter_intra, enable_intra_edge_filter
    put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1);   // interintra, masked compound, warped motion, dual filter
    put(&b, 1, 1); put(&b, 0, 1); put(&b, 0, 1);               // enable_order_hint, enable_jnt_comp, enable_ref_frame_mvs
    put(&b, 0, 1); put(&b, 0, 1);                              // seq_choose_screen_content_tools, seq_force_screen_content_tools 0
    put(&b, (unsigned)(s->order_hint_bits ? s->order_hint_bits : 7) - 1, 3);
    put(&b, 0, 1); put(&b, s->enable_cdef, 1); put(&b, s->enable_restoration, 1);   // enable_superres, cdef, restoration
    // color_config (5.5.2): profile 0
    put(&b, s->bit_depth > 8, 1); put(&b, 0, 1);               // high_bitdepth, mono_chrome
    put(&b, 1, 1); for (int i = 0; i < 3; i++) put(&b, (unsigned)(s->colour[i] ? s->colour[i] : 1), 8);   // color description
    put(&b, s->full_range, 1); put(&b, 0, 2); put(&b, 0, 1);   // color_range, chroma_sample_position unknown, separate_uv_delta_q
    put(&b, 0, 1);                                             // film_grain_params_present
    trailing(&b);
    return obu(OBU_SEQUENCE_HEADER, p, b.bits / 8, out);
}

static int tile_log2(int blk, int target) { int k = 0; while ((blk << k) < target) k++; return k; }   // 5.9.15

static void delta_q(Bw *b, int v) { put(b, v != 0, 1); if (v) su(b, v, 7); }           // read_delta_q, 5.9.13

size_t av1_write_frame(const Av1SeqParams *s, const Av1FrameParams *f, const uint8_t *const *tile, const size_t *tile_size,
                       int ntiles, uint8_t *out)
{
    // the header goes at the start of out (at most a few hundred bytes); the OBU header is put in front at the end
    uint8_t *p = out + 16; size_t total = 0;
    for (int i = 0; i < ntiles; i++) total += tile_size[i] + 4;
    memset(p, 0, 512);
    Bw b = { p, 0 };
    const bool key = f->frame_type == 0, intra = key;
    const int ohb = s->order_hint_bits ? s->order_hint_bits : 7;
    // uncompressed_header (5.9.2); show_existing_frame 0
    put(&b, 0, 1); put(&b, (unsigned)f->frame_type, 2); put(&b, !f->hidden, 1);   // show_existing_frame, frame_type, show_frame
    if (f->hidden) put(&b, 1, 1);                              // showable_frame
    const bool er = key ? true : f->error_resilient;           // a shown key frame is error resilient (not signalled)
    if (!key) put(&b, f->error_resilient, 1);
    put(&b, f->disable_cdf_update, 1);
    // allow_screen_content_tools 0 (seq_force_screen_content_tools 0): no bits; frame_size_override_flag
    put(&b, 0, 1);
    put(&b, (unsigned)f->order_hint & ((1u << ohb) - 1), ohb);
    if (!(intra || er)) put(&b, (unsigned)f->primary_ref_frame, 3);
    if (!key) put(&b, (unsigned)f->refresh_frame_flags, 8);
    if (!intra && er && f->refresh_frame_flags != 0xff) for (int i = 0; i < 8; i++) put(&b, 0, ohb);   // ref_order_hint[] (unused: no ER inter frames)
    if (key) {
        put(&b, 0, 1);                                         // frame_size(): no override; render_and_frame_size_different
    } else {
        put(&b, 0, 1);                                         // frame_refs_short_signaling
        for (int i = 0; i < 7; i++) put(&b, (unsigned)f->ref_frame_idx[i], 3);
        put(&b, 0, 1);                                         // frame_size() (no override), render_and_frame_size_different
        put(&b, f->allow_high_precision_mv, 1);                // (force_integer_mv 0)
        put(&b, f->interp_filter == 4, 1); if (f->interp_filter != 4) put(&b, (unsigned)f->interp_filter, 2);
        put(&b, f->is_motion_mode_switchable, 1);
        // use_ref_frame_mvs: 0 (enable_ref_frame_mvs 0)
    }
    if (!f->disable_cdf_update) put(&b, f->disable_frame_end_update_cdf, 1);
    // tile_info (5.9.15), uniform spacing
    const int miCols = 2 * ((s->width + 7) >> 3), miRows = 2 * ((s->height + 7) >> 3);
    const int sbCols = (miCols + 15) >> 4, sbRows = (miRows + 15) >> 4;
    const int maxTileAreaSb = (4096 * 2304) >> 12;
    const int minLog2TileCols = tile_log2(64, sbCols), maxLog2TileCols = tile_log2(1, sbCols < 64 ? sbCols : 64);
    const int maxLog2TileRows = tile_log2(1, sbRows < 64 ? sbRows : 64);
    const int minLog2Tiles = minLog2TileCols > tile_log2(maxTileAreaSb, sbRows * sbCols) ? minLog2TileCols : tile_log2(maxTileAreaSb, sbRows * sbCols);
    put(&b, 1, 1);                                             // uniform_tile_spacing_flag
    int cl = minLog2TileCols;
    while (cl < maxLog2TileCols) { const int inc = cl < f->tile_cols_log2; put(&b, (unsigned)inc, 1); if (!inc) break; cl++; }
    int rl = minLog2Tiles - cl > 0 ? minLog2Tiles - cl : 0;
    while (rl < maxLog2TileRows) { const int inc = rl < f->tile_rows_log2; put(&b, (unsigned)inc, 1); if (!inc) break; rl++; }
    if (cl || rl) { put(&b, (unsigned)f->context_update_tile_id, cl + rl); put(&b, (unsigned)f->tile_size_bytes - 1, 2); }
    // quantization_params (5.9.12)
    put(&b, (unsigned)f->base_q_idx, 8);
    delta_q(&b, f->dq_y_dc); delta_q(&b, f->dq_u_dc); delta_q(&b, f->dq_u_ac);   // (separate_uv_delta_q 0)
    put(&b, 0, 1);                                             // using_qmatrix
    put(&b, 0, 1);                                             // segmentation_enabled
    if (f->base_q_idx > 0) {                                   // delta_q_params, delta_lf_params (5.9.17, 5.9.18)
        put(&b, f->delta_q_present, 1);
        if (f->delta_q_present) { put(&b, 0, 2); put(&b, 0, 1); }   // delta_q_res 0, delta_lf_present 0
    }
    const bool lossless = f->base_q_idx == 0 && !f->dq_y_dc && !f->dq_u_dc && !f->dq_u_ac;
    if (!lossless) {                                           // loop_filter_params (5.9.11)
        put(&b, (unsigned)f->lf_level[0], 6); put(&b, (unsigned)f->lf_level[1], 6);
        if (f->lf_level[0] || f->lf_level[1]) { put(&b, (unsigned)f->lf_level[2], 6); put(&b, (unsigned)f->lf_level[3], 6); }
        put(&b, (unsigned)f->lf_sharpness, 3);
        put(&b, f->lf_delta_enabled, 1); if (f->lf_delta_enabled) put(&b, 0, 1);   // loop_filter_delta_update 0
        if (s->enable_cdef) {                                  // cdef_params (5.9.19)
            put(&b, (unsigned)f->cdef_damping - 3, 2); put(&b, (unsigned)f->cdef_bits, 2);
            for (int i = 0; i < (1 << f->cdef_bits); i++) {
                put(&b, (unsigned)f->cdef_y[i] >> 2, 4); put(&b, (unsigned)f->cdef_y[i] & 3, 2);
                put(&b, (unsigned)f->cdef_uv[i] >> 2, 4); put(&b, (unsigned)f->cdef_uv[i] & 3, 2);
            }
        }
        if (s->enable_restoration) {                           // lr_params (5.9.20), 64x64 superblocks
            for (int i = 0; i < 3; i++) put(&b, (unsigned)f->lr_type[i], 2);
            if (f->lr_type[0] || f->lr_type[1] || f->lr_type[2]) {
                put(&b, f->lr_unit_shift > 0, 1); if (f->lr_unit_shift > 0) put(&b, f->lr_unit_shift > 1, 1);
                if (f->lr_type[1] || f->lr_type[2]) put(&b, 0, 1); // lr_uv_shift (4:2:0)
            }
        }
        put(&b, f->tx_mode_select, 1);                         // read_tx_mode
    }
    if (!intra) put(&b, f->reference_select, 1);               // frame_reference_mode
    if (!intra && f->reference_select) {                       // skip_mode_params (5.9.22): skip_mode_present 0 when allowed
        const int m = 1 << (ohb - 1);
        #define REL(a, b2) ((((a) - (b2)) & (m - 1)) - (((a) - (b2)) & m))
        int fwd = -1, fwdHint = 0, bwd = -1, bwdHint = 0, fwd2 = -1, fwd2Hint = 0;
        for (int i = 0; i < 7; i++) {
            const int h = f->ref_order_hint[i] & ((1 << ohb) - 1), oh = f->order_hint & ((1 << ohb) - 1);
            if (REL(h, oh) < 0) { if (fwd < 0 || REL(h, fwdHint) > 0) { fwd = i; fwdHint = h; } }
            else if (REL(h, oh) > 0) { if (bwd < 0 || REL(h, bwdHint) < 0) { bwd = i; bwdHint = h; } }
        }
        bool allowed = fwd >= 0 && bwd >= 0;
        if (fwd >= 0 && bwd < 0) {
            for (int i = 0; i < 7; i++) {
                const int h = f->ref_order_hint[i] & ((1 << ohb) - 1);
                if (REL(h, fwdHint) < 0 && (fwd2 < 0 || REL(h, fwd2Hint) > 0)) { fwd2 = i; fwd2Hint = h; }
            }
            allowed = fwd2 >= 0;
        }
        #undef REL
        if (allowed) put(&b, 0, 1);
    }
    // allow_warped_motion: 0 (sequence)
    put(&b, f->reduced_tx_set, 1);
    if (!intra) for (int i = 0; i < 7; i++) put(&b, 0, 1);     // global_motion_params: is_global 0
    // film_grain_params: none (sequence)
    align(&b);                                                 // byte_alignment() after the frame header (5.9.1)
    // tile_group_obu (5.11.1): no tile_start_and_end_present_flag for one tile group
    if (ntiles > 1) { put(&b, 0, 1); align(&b); }
    size_t n = b.bits / 8;
    for (int i = 0; i < ntiles; i++) {
        if (i < ntiles - 1) { const size_t sz = tile_size[i] - 1; for (int k = 0; k < f->tile_size_bytes; k++) p[n++] = (uint8_t)(sz >> (8 * k)); }
        memcpy(p + n, tile[i], tile_size[i]); n += tile_size[i];
    }
    (void)total;
    return obu(OBU_FRAME, p, n, out);
}

size_t av1_write_show_existing(int slot, uint8_t *out)   // 5.9.2 with show_existing_frame (no decoder model, no film grain)
{
    uint8_t p[4] = { 0 }; Bw b = { p, 0 };
    put(&b, 1, 1); put(&b, (unsigned)slot, 3);                 // show_existing_frame, frame_to_show_map_idx
    trailing(&b);
    return obu(OBU_FRAME_HEADER, p, b.bits / 8, out);
}

size_t av1_write_hdr_metadata(const uint8_t *mdcv, const uint8_t *cll, uint8_t *out)   // 5.8.4 / 5.8.5, 6.7.4 / 6.7.3
{
    size_t n = 0;
    #define BE16(p) ((unsigned)(p)[0] << 8 | (p)[1])
    #define BE32(p) ((uint32_t)(p)[0] << 24 | (uint32_t)(p)[1] << 16 | (uint32_t)(p)[2] << 8 | (p)[3])
    if (mdcv) {   // SEI: G B R primaries, white point (0.00002), max / min luminance (0.0001 cd/m2); AV1: R G B, 0.16, 24.8, 18.14
        uint8_t p[32] = { 0 }; Bw b = { p, 0 };
        put(&b, 2, 8);                                         // metadata_type HDR_MDCV (leb128, one byte)
        static const int order[3] = { 2, 0, 1 };              // R G B from the SEI's G B R
        for (int i = 0; i < 3; i++) {
            put(&b, (unsigned)(((uint64_t)BE16(mdcv + 4 * order[i]) * 131072ull + 50000) / 100000u), 16);
            put(&b, (unsigned)(((uint64_t)BE16(mdcv + 4 * order[i] + 2) * 131072ull + 50000) / 100000u), 16);
        }
        put(&b, (unsigned)(((uint64_t)BE16(mdcv + 12) * 131072ull + 50000) / 100000u), 16); put(&b, (unsigned)(((uint64_t)BE16(mdcv + 14) * 131072ull + 50000) / 100000u), 16);
        put(&b, (unsigned)(((uint64_t)BE32(mdcv + 16) * 256 + 5000) / 10000), 32); put(&b, (unsigned)(((uint64_t)BE32(mdcv + 20) * 16384 + 5000) / 10000), 32);
        trailing(&b);
        n += obu(OBU_METADATA, p, b.bits / 8, out + n);
    }
    if (cll) {
        uint8_t p[8] = { 0 }; Bw b = { p, 0 };
        put(&b, 1, 8); put(&b, BE16(cll), 16); put(&b, BE16(cll + 2), 16);   // metadata_type HDR_CLL, max_cll, max_fall
        trailing(&b);
        n += obu(OBU_METADATA, p, b.bits / 8, out + n);
    }
    #undef BE16
    #undef BE32
    return n;
}
