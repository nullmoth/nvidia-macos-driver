// h264parse.h: H.264 bitstream reader for the decoder (M8): NAL units (Annex B or length-prefixed), SPS, PPS, the slice
// header up to dec_ref_pic_marking, and picture order count. NVDEC parses the slice data itself; the driver supplies the
// picture-level setup (sequence and picture parameters, the DPB with its POCs, the current picture), so this reads what
// that setup and the DPB management need. From the H.264 standard (ITU-T H.264, 7.3 / 7.4 / 8.2.1).
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum { H264_NAL_SLICE = 1, H264_NAL_IDR = 5, H264_NAL_SEI = 6, H264_NAL_SPS = 7, H264_NAL_PPS = 8, H264_NAL_AUD = 9,
       H264_NAL_EOSEQ = 10, H264_NAL_EOSTREAM = 11 };
enum { H264_SLICE_P = 0, H264_SLICE_B = 1, H264_SLICE_I = 2, H264_SLICE_SP = 3, H264_SLICE_SI = 4 };

typedef struct {
    bool valid;
    int profile_idc, constraint_flags, level_idc, sps_id;
    int chroma_format_idc, separate_colour_plane_flag, bit_depth_luma_minus8, bit_depth_chroma_minus8;
    int qpprime_y_zero_transform_bypass_flag, seq_scaling_matrix_present_flag;
    uint8_t scaling4x4[6][16], scaling8x8[6][64];    // in zig-zag (coded) order, after the fall-back rules
    int log2_max_frame_num_minus4, pic_order_cnt_type, log2_max_pic_order_cnt_lsb_minus4;
    int delta_pic_order_always_zero_flag, offset_for_non_ref_pic, offset_for_top_to_bottom_field;
    int num_ref_frames_in_pic_order_cnt_cycle, offset_for_ref_frame[256];
    int max_num_ref_frames, gaps_in_frame_num_value_allowed_flag;
    int pic_width_in_mbs_minus1, pic_height_in_map_units_minus1, frame_mbs_only_flag, mb_adaptive_frame_field_flag;
    int direct_8x8_inference_flag, frame_cropping_flag, crop_left, crop_right, crop_top, crop_bottom;
    int vui_present, video_full_range_flag, colour_primaries, transfer_characteristics, matrix_coefficients;
    int bitstream_restriction_flag, max_num_reorder_frames, max_dec_frame_buffering;
    int width, height;                                // luma samples after cropping
    int crop_x0, crop_y0;           // the cropped picture's origin in luma samples (frame_crop_left / top_offset)
} H264Sps;

typedef struct {
    bool valid;
    int pps_id, sps_id, entropy_coding_mode_flag, bottom_field_pic_order_in_frame_present_flag, num_slice_groups_minus1;
    int num_ref_idx_l0_default_active_minus1, num_ref_idx_l1_default_active_minus1, weighted_pred_flag, weighted_bipred_idc;
    int pic_init_qp_minus26, pic_init_qs_minus26, chroma_qp_index_offset, deblocking_filter_control_present_flag;
    int constrained_intra_pred_flag, redundant_pic_cnt_present_flag, transform_8x8_mode_flag;
    int pic_scaling_matrix_present_flag, second_chroma_qp_index_offset;
    uint8_t scaling4x4[6][16], scaling8x8[6][64];    // effective lists for pictures using this PPS (zig-zag order)
} H264Pps;

typedef struct { int op, difference_of_pic_nums_minus1, long_term_pic_num, long_term_frame_idx, max_long_term_frame_idx_plus1; } H264Mmco;

typedef struct {
    int nal_unit_type, nal_ref_idc, idr;
    int first_mb_in_slice, slice_type, pps_id, colour_plane_id, frame_num, field_pic_flag, bottom_field_flag, idr_pic_id;
    int pic_order_cnt_lsb, delta_pic_order_cnt_bottom, delta_pic_order_cnt[2], redundant_pic_cnt;
    int direct_spatial_mv_pred_flag, num_ref_idx_l0_active_minus1, num_ref_idx_l1_active_minus1;
    int no_output_of_prior_pics_flag, long_term_reference_flag, adaptive_ref_pic_marking_mode_flag;
    int n_mmco; H264Mmco mmco[66];
    int has_mmco5;                                    // memory_management_control_operation 5 present
    int nonexist_marker;                              // set by the decoder for frame_num gap frames (not parsed)
} H264Slice;

typedef struct { H264Sps sps[32]; H264Pps pps[256]; } H264Params;

typedef struct {   // state for picture order count (8.2.1), kept between pictures
    int prev_poc_msb, prev_poc_lsb, prev_frame_num_offset, prev_frame_num, prev_ref_has_mmco5, prev_ref_bottom_field;
    int prev_ref_top_poc;                             // TopFieldOrderCnt of the previous reference picture with mmco 5
} H264PocState;

// Next NAL unit in an Annex B stream (start codes). Returns false at the end. *nal/*len: the NAL unit without start code.
bool h264_next_annexb(const uint8_t *buf, size_t size, size_t *pos, const uint8_t **nal, size_t *len);
// Next NAL unit with an n-byte big-endian length prefix (VideoToolbox / MP4 avcC samples, n = 1, 2 or 4).
bool h264_next_lenpfx(const uint8_t *buf, size_t size, int n, size_t *pos, const uint8_t **nal, size_t *len);

// Parse one NAL unit into the parameter sets (SPS, PPS) or a slice header. Returns 0 on success, -1 on a malformed or
// unsupported unit (slice groups, unknown parameter set). For slices *out is filled; parameter sets update *p.
int h264_parse_sps(H264Params *p, const uint8_t *nal, size_t len);
int h264_parse_pps(H264Params *p, const uint8_t *nal, size_t len);
int h264_parse_slice_header(const H264Params *p, const uint8_t *nal, size_t len, H264Slice *out);

// Picture order count of the picture whose first slice is s (8.2.1); updates the state after a reference picture.
// top/bottom: TopFieldOrderCnt / BottomFieldOrderCnt (the one not coded for a field picture is set equal).
void h264_poc(H264PocState *st, const H264Sps *sps, const H264Slice *s, int *top, int *bottom);

// Zig-zag (frame scan) to raster position, for the scaling matrices NVDEC takes in raster order.
extern const uint8_t h264_zigzag4x4[16], h264_zigzag8x8[64];
// 7.4.1.2.4: does this slice start a new primary coded picture after prev? (Arbitrary slice order puts slices with
// first_mb_in_slice != 0 first, so first_mb_in_slice == 0 alone is not enough.)
bool h264_new_picture(const H264Params *p, const H264Slice *prev, const H264Slice *cur);
