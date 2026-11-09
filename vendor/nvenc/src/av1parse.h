// av1parse.h (M9): AV1 bitstream reader for the decoder: OBUs, the sequence header, the uncompressed frame header with
// everything it depends on from earlier frames (reference slots: sizes, order hints, loop filter deltas, segmentation
// features, global motion, film grain), and tile groups (where each tile's data is). NVDEC decodes the tiles; the driver
// supplies the picture setup from these headers. From the AV1 Bitstream & Decoding Process Specification (sections 5.3,
// 5.5, 5.9, 5.11.1, 7.8, 7.20, 7.21).
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum { AV1_OBU_SEQUENCE_HEADER = 1, AV1_OBU_TEMPORAL_DELIMITER = 2, AV1_OBU_FRAME_HEADER = 3, AV1_OBU_TILE_GROUP = 4,
       AV1_OBU_METADATA = 5, AV1_OBU_FRAME = 6, AV1_OBU_REDUNDANT_FRAME_HEADER = 7, AV1_OBU_TILE_LIST = 8, AV1_OBU_PADDING = 15 };
enum { AV1_KEY_FRAME = 0, AV1_INTER_FRAME = 1, AV1_INTRA_ONLY_FRAME = 2, AV1_SWITCH_FRAME = 3 };
enum { AV1_PRIMARY_REF_NONE = 7, AV1_SWITCHABLE = 4 };
enum { AV1_RESTORE_NONE = 0, AV1_RESTORE_WIENER = 1, AV1_RESTORE_SGRPROJ = 2, AV1_RESTORE_SWITCHABLE = 3 };
enum { AV1_ONLY_4X4 = 0, AV1_TX_MODE_LARGEST = 1, AV1_TX_MODE_SELECT = 2 };
enum { AV1_IDENTITY = 0, AV1_TRANSLATION = 1, AV1_ROTZOOM = 2, AV1_AFFINE = 3 };
enum { AV1_MAX_TILES = 4096 };

typedef struct {
    int type, extension, temporal_id, spatial_id, has_size;
    size_t header_len, payload_len;     // the payload starts header_len bytes into the OBU
} Av1Obu;

typedef struct {
    bool valid;
    int profile, still_picture, reduced_still_picture_header;
    int timing_info_present, decoder_model_info_present, equal_picture_interval;
    int buffer_removal_time_length_minus_1, frame_presentation_time_length_minus_1;
    int operating_points_cnt_minus_1, operating_point_idc[32], seq_level_idx[32], decoder_model_present_for_this_op[32];
    int frame_width_bits_minus_1, frame_height_bits_minus_1, max_frame_width_minus_1, max_frame_height_minus_1;
    int frame_id_numbers_present, delta_frame_id_length_minus_2, additional_frame_id_length_minus_1;
    int use_128x128_superblock, enable_filter_intra, enable_intra_edge_filter, enable_interintra_compound;
    int enable_masked_compound, enable_warped_motion, enable_dual_filter, enable_order_hint, enable_jnt_comp;
    int enable_ref_frame_mvs, seq_force_screen_content_tools, seq_force_integer_mv, order_hint_bits;
    int enable_superres, enable_cdef, enable_restoration;
    int bit_depth, mono_chrome, color_description_present, color_primaries, transfer_characteristics, matrix_coefficients;
    int color_range, subsampling_x, subsampling_y, chroma_sample_position, separate_uv_delta_q;
    int film_grain_params_present;
} Av1Seq;

typedef struct {
    int apply_grain, grain_seed, update_grain, film_grain_params_ref_idx;
    int num_y_points, point_y_value[16], point_y_scaling[16], chroma_scaling_from_luma;
    int num_cb_points, point_cb_value[16], point_cb_scaling[16], num_cr_points, point_cr_value[16], point_cr_scaling[16];
    int grain_scaling_minus_8, ar_coeff_lag, ar_coeffs_y_plus_128[24], ar_coeffs_cb_plus_128[25], ar_coeffs_cr_plus_128[25];
    int ar_coeff_shift_minus_6, grain_scale_shift, cb_mult, cb_luma_mult, cb_offset, cr_mult, cr_luma_mult, cr_offset;
    int overlap_flag, clip_to_restricted_range;
} Av1FilmGrain;

typedef struct {
    int show_existing_frame, frame_to_show_map_idx;
    int frame_type, frame_is_intra, show_frame, showable_frame, error_resilient_mode;
    int disable_cdf_update, allow_screen_content_tools, force_integer_mv, current_frame_id, frame_size_override_flag;
    int order_hint, primary_ref_frame, refresh_frame_flags;
    int frame_width, frame_height, upscaled_width, render_width, render_height, use_superres, superres_denom;
    int mi_cols, mi_rows;
    int allow_intrabc, frame_refs_short_signaling, ref_frame_idx[7], allow_high_precision_mv, interpolation_filter;
    int is_motion_mode_switchable, use_ref_frame_mvs;
    int order_hints[8], ref_frame_sign_bias[8];          // indexed by reference frame (LAST_FRAME = 1 .. ALTREF_FRAME = 7)
    int disable_frame_end_update_cdf;
    // tiles
    int uniform_tile_spacing, tile_cols, tile_rows, tile_cols_log2, tile_rows_log2, context_update_tile_id, tile_size_bytes;
    int mi_col_starts[65], mi_row_starts[65];
    // quantisation, segmentation, deltas
    int base_q_idx, delta_q_y_dc, delta_q_u_dc, delta_q_u_ac, delta_q_v_dc, delta_q_v_ac, diff_uv_delta;
    int using_qmatrix, qm_y, qm_u, qm_v;
    int seg_enabled, seg_update_map, seg_temporal_update, seg_update_data;
    int seg_feature_enabled[8][8], seg_feature_data[8][8], seg_id_pre_skip, last_active_seg_id;
    int delta_q_present, delta_q_res, delta_lf_present, delta_lf_res, delta_lf_multi;
    int coded_lossless, all_lossless, lossless[8];
    // loop filter, CDEF, loop restoration
    int lf_level[4], lf_sharpness, lf_delta_enabled, lf_delta_update, lf_ref_deltas[8], lf_mode_deltas[2];
    int cdef_damping, cdef_bits, cdef_y_pri[8], cdef_y_sec[8], cdef_uv_pri[8], cdef_uv_sec[8];
    int lr_type[3], uses_lr, lr_unit_shift, lr_uv_shift, lr_size[3];   // lr_type: FrameRestorationType (RESTORE_*)
    // modes
    int tx_mode, reference_select, skip_mode_present, skip_mode_frame[2], allow_warped_motion, reduced_tx_set;
    int gm_type[8], gm_params[8][6];
    Av1FilmGrain fg;
    int temporal_id, spatial_id;
} Av1Frame;

typedef struct {
    int valid, frame_id, upscaled_width, frame_width, frame_height, render_width, render_height, mi_cols, mi_rows;
    int frame_type, order_hint, saved_order_hints[8], bit_depth, subsampling_x, subsampling_y, showable;
    int gm_params[8][6], lf_ref_deltas[8], lf_mode_deltas[2], seg_feature_enabled[8][8], seg_feature_data[8][8];
    Av1FilmGrain fg;
} Av1Ref;

typedef struct {
    Av1Seq seq;
    Av1Ref ref[8];
    int temporal_id, spatial_id;    // of the OBU being parsed
} Av1State;

typedef struct { size_t offset, size; int row, col; } Av1Tile;   // offset into the tile group OBU's payload

// One OBU header at p (n bytes left); 0, or -1 if malformed / truncated.
int  av1_obu(const uint8_t *p, size_t n, Av1Obu *o);
int  av1_parse_sequence_header(Av1Seq *s, const uint8_t *p, size_t n);
// The uncompressed header of a frame header or frame OBU payload. *bytes = its size, byte-aligned (for a frame OBU the
// tile group starts there). Applies setup_past_independence / load_previous. Returns 0 or -1.
int  av1_parse_frame_header(Av1State *st, const uint8_t *p, size_t n, Av1Frame *f, size_t *bytes);
// A tile group (payload p of n bytes): tiles tg_start..tg_end with their data ranges. Returns the number of tiles, *last
// = whether this group ends the frame, or -1.
int  av1_parse_tile_group(const Av1Frame *f, const uint8_t *p, size_t n, Av1Tile *tiles, int max, int *tg_start, bool *last);
// After a frame (or a show_existing_frame of a key frame): the reference frame update process (7.20; 7.21 first for a
// shown key frame).
void av1_end_frame(Av1State *st, Av1Frame *f);
static inline int av1_get_relative_dist(const Av1Seq *s, int a, int b)
{
    if (!s->enable_order_hint) return 0;
    int diff = a - b, m = 1 << (s->order_hint_bits - 1);
    return (diff & (m - 1)) - (diff & m);
}
