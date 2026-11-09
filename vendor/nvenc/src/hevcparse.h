// hevcparse.h (M11): HEVC bitstream reader for the decoder: NAL units, SPS (with short-term reference picture sets,
// scaling lists, VUI), PPS (tiles, range extension), the slice segment header and picture order count. NVDEC parses the
// slice data (and, given bit counts, the slice header) itself; the driver supplies the picture-level setup and the
// reference picture set. From ITU-T H.265 (7.3, 7.4, 8.3.1).
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum { HEVC_NAL_TRAIL_N = 0, HEVC_NAL_RASL_R = 9, HEVC_NAL_BLA_W_LP = 16, HEVC_NAL_IDR_W_RADL = 19, HEVC_NAL_IDR_N_LP = 20,
       HEVC_NAL_CRA = 21, HEVC_NAL_RSV_IRAP_23 = 23, HEVC_NAL_VPS = 32, HEVC_NAL_SPS = 33, HEVC_NAL_PPS = 34, HEVC_NAL_AUD = 35,
       HEVC_NAL_EOS = 36, HEVC_NAL_EOB = 37, HEVC_NAL_FD = 38, HEVC_NAL_SEI_PREFIX = 39, HEVC_NAL_SEI_SUFFIX = 40 };
enum { HEVC_SLICE_B = 0, HEVC_SLICE_P = 1, HEVC_SLICE_I = 2 };

typedef struct {
    int num_negative, num_positive;                 // NumNegativePics, NumPositivePics
    int delta_poc[32], used[32];                    // S0 (negative, closest first) then S1 (positive, closest first)
} HevcStRps;

typedef struct {
    uint8_t list[4][6][64];                         // ScalingFactor coefficients in coded (up-right diagonal) order
    uint8_t dc[2][6];                               // 16x16 and 32x32 DC values
} HevcScaling;

typedef struct {
    bool valid;
    int sps_id, vps_id, max_sub_layers_minus1;
    int general_profile_idc, general_level_idc;
    int chroma_format_idc, separate_colour_plane_flag, width, height;
    int conf_win_left, conf_win_right, conf_win_top, conf_win_bottom;   // in luma samples
    int bit_depth_luma, bit_depth_chroma, log2_max_poc_lsb;
    int max_dec_pic_buffering[8], max_num_reorder[8], max_latency_increase_plus1[8];
    int log2_min_cb, log2_max_cb, log2_min_tb, log2_max_tb, max_th_depth_inter, max_th_depth_intra;
    int scaling_list_enabled, scaling_list_data_present; HevcScaling scaling;
    int amp_enabled, sao_enabled, pcm_enabled, pcm_bit_depth_luma, pcm_bit_depth_chroma, log2_min_pcm_cb, log2_max_pcm_cb;
    int pcm_loop_filter_disabled;
    int num_short_term_ref_pic_sets; HevcStRps st_rps[65];
    int long_term_ref_pics_present, num_long_term_ref_pics_sps, lt_ref_pic_poc_lsb_sps[33], used_by_curr_pic_lt_sps[33];
    int temporal_mvp_enabled, strong_intra_smoothing_enabled;
    int vui_present, video_full_range_flag, colour_primaries, transfer_characteristics, matrix_coeffs;
    // range extension
    int transform_skip_rotation_enabled, transform_skip_context_enabled, implicit_rdpcm_enabled, explicit_rdpcm_enabled;
    int extended_precision_processing, intra_smoothing_disabled, high_precision_offsets_enabled, persistent_rice_adaptation_enabled;
    int cabac_bypass_alignment_enabled;
    int end_bit;                                    // RBSP bits up to rbsp_trailing_bits() (0: unknown, extension data follows)
} HevcSps;

typedef struct {
    bool valid;
    int pps_id, sps_id, dependent_slice_segments_enabled, output_flag_present, num_extra_slice_header_bits;
    int sign_data_hiding_enabled, cabac_init_present, num_ref_idx_l0_default_active, num_ref_idx_l1_default_active;
    int init_qp_minus26, constrained_intra_pred, transform_skip_enabled, cu_qp_delta_enabled, diff_cu_qp_delta_depth;
    int cb_qp_offset, cr_qp_offset, slice_chroma_qp_offsets_present, weighted_pred, weighted_bipred, transquant_bypass_enabled;
    int tiles_enabled, entropy_coding_sync_enabled, num_tile_columns, num_tile_rows, uniform_spacing;
    int column_width[21], row_height[23];           // in CTBs (derived for uniform spacing at SPS activation: hevc_tiles())
    int loop_filter_across_tiles, loop_filter_across_slices, deblocking_filter_control_present;
    int deblocking_filter_override_enabled, pps_deblocking_filter_disabled, beta_offset_div2, tc_offset_div2;
    int scaling_list_data_present; HevcScaling scaling;
    int lists_modification_present, log2_parallel_merge_level, slice_segment_header_extension_present;
    // range extension
    int log2_max_transform_skip_size, cross_component_prediction_enabled, chroma_qp_offset_list_enabled;
    int diff_cu_chroma_qp_offset_depth, chroma_qp_offset_list_len, cb_qp_offset_list[6], cr_qp_offset_list[6];
    int log2_sao_offset_scale_luma, log2_sao_offset_scale_chroma;
    int end_bit;
} HevcPps;

typedef struct { HevcSps sps[16]; HevcPps pps[64]; } HevcParams;

typedef struct {
    int nal_type, temporal_id;
    int first_slice_segment_in_pic, no_output_of_prior_pics, pps_id, dependent_slice_segment, slice_segment_address;
    int slice_type, pic_output_flag, colour_plane_id, poc_lsb;
    int short_term_ref_pic_set_sps_flag, short_term_ref_pic_set_idx; HevcStRps st_rps;   // the RPS in use
    int st_rps_bits;                                 // bits of short_term_ref_pic_set() in the slice header (0 if from the SPS)
    int st_rps_ref_num_delta;                        // NumDeltaPocs[RefRpsIdx] when that set is predicted (inter RPS), else 0
    int num_long_term_sps, num_long_term_pics, lt_poc_lsb[32], lt_used[32], lt_msb_present[32], lt_delta_msb_cycle[32];
    int rps_start, rps_bits;                         // RBSP bit (after the NAL header) of short_term_ref_pic_set_sps_flag, and the
                                                     // bits from it to the end of the long-term part
    int slice_temporal_mvp_enabled, sao_luma, sao_chroma;
    int num_ref_idx_active[2], ref_pic_list_modification[2], list_entry[2][16];
    int mvd_l1_zero, cabac_init, collocated_from_l0, collocated_ref_idx, max_num_merge_cand;
    int slice_qp_delta, slice_cb_qp_offset, slice_cr_qp_offset, cu_chroma_qp_offset_enabled;
    int deblocking_override, deblocking_disabled, beta_offset_div2, tc_offset_div2, loop_filter_across_slices;
    int num_entry_point_offsets, offset_len_minus1;
    int header_bits;                                 // RBSP bits of the whole slice segment header
    int header_bytes;                                // and bytes of the NAL unit (with its 2-byte header and emulation bytes)
} HevcSlice;

typedef struct { int prev_poc_tid0; bool first; } HevcPocState;

// NAL units of an Annex B buffer / of length-prefixed samples (as h264parse's).
bool hevc_next_annexb(const uint8_t *buf, size_t n, size_t *pos, const uint8_t **nal, size_t *len);
bool hevc_next_lenpfx(const uint8_t *buf, size_t n, int nal_len, size_t *pos, const uint8_t **nal, size_t *len);
// SPS / PPS: return its id, or -1
int  hevc_parse_sps(HevcParams *p, const uint8_t *nal, size_t len);
int  hevc_parse_pps(HevcParams *p, const uint8_t *nal, size_t len);
int  hevc_parse_slice_header(const HevcParams *p, const uint8_t *nal, size_t len, HevcSlice *s);
// Tile column widths / row heights in CTBs (uniform spacing is derived from the SPS)
void hevc_tiles(const HevcSps *sps, const HevcPps *pps, int *cols, int *rows);
static inline bool hevc_is_irap(int t) { return t >= HEVC_NAL_BLA_W_LP && t <= HEVC_NAL_RSV_IRAP_23; }
static inline bool hevc_is_idr(int t) { return t == HEVC_NAL_IDR_W_RADL || t == HEVC_NAL_IDR_N_LP; }
// PicOrderCntVal (8.3.1); no_rasl_output = the picture is an IDR / BLA, or the first picture / a CRA after an EOS.
int  hevc_poc(HevcPocState *st, const HevcSps *sps, const HevcSlice *s, bool no_rasl_output);
// Default scaling lists (7.4.5) into a HevcScaling (sizeId 0: flat 16; 1..3: the intra / inter defaults)
void hevc_default_scaling(HevcScaling *sl);
