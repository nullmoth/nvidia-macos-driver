// h264hdr.h: H.264 SPS/PPS NAL units (ITU-T H.264 7.3.2.1 / 7.3.2.2), matching what we tell NVENC in
// nvenc_h264_sps_data_s / nvenc_h264_pps_data_s, so the slices the engine writes decode against them.
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct {
    int width, height;              // display size; coded size is rounded up to whole macroblocks (cropping signalled)
    int profile_idc, level_idc;     // e.g. 77 main / 100 high, 51 = level 5.1
    int log2_max_frame_num_minus4, log2_max_poc_lsb_minus4;
    int poc_type;                   // 0 or 2
    int num_reorder;                // VUI max_num_reorder_frames (B pictures: 1)
    int num_ref_idx_l0_minus1;      // PPS default
    int num_ref_frames;
    int entropy_cabac;              // 0 CAVLC, 1 CABAC
    int pic_init_qp;                // 26 + pic_init_qp_minus26
    int transform_8x8;              // high profile only
    int vui;                        // write VUI: colour description, frame reordering (num_reorder)
    int full_range;                 // VUI video_full_range_flag
    int colour[3];                  // VUI colour_primaries, transfer_characteristics, matrix_coefficients (H.273); 0 = BT.709 (1)
    int chroma444;                  // High 4:4:4 Predictive (profile_idc 244): chroma_format_idc 3
    int lossless;                   // qpprime_y_zero_transform_bypass_flag (needs profile_idc 244; QP 0 macroblocks are lossless)
} H264Params;

// Annex B (start code 00 00 00 01) SPS and PPS; returns bytes written into out (needs ~64 bytes)
// SEI NAL unit (H.264 type 6 / HEVC prefix SEI 39) with HDR metadata: mastering_display_colour_volume (24 bytes, the SEI
// payload as VideoToolbox's MasteringDisplayColorVolume holds it) and / or content_light_level_info (4 bytes); either may
// be NULL. Annex B, emulation prevention applied; returns bytes written (< 64)
size_t hdr_write_sei(bool hevc, const uint8_t *mdcv, const uint8_t *cll, uint8_t *out);
size_t h264_write_sps(const H264Params *p, uint8_t *out);
size_t h264_write_pps(const H264Params *p, uint8_t *out);
