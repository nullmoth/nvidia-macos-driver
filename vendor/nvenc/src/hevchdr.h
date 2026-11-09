// hevchdr.h: HEVC VPS/SPS/PPS NAL units (ITU-T H.265 7.3.2.1-7.3.2.3), matching what we tell NVENC in
// nvenc_h265_sps_data_s / nvenc_h265_pps_data_s, so the slice segments the engine writes decode against them.
// Values follow what nullmoth's libnvenc_hevc writes (docs/reports/T6.03.md): Main profile, CTB 32, CB 16..32,
// TB 4..32, AMP + SAO, one short-term RPS in the SPS (slices send their own), no temporal MVP. Unlike nullmoth's, no
// sign data hiding (no_sdh): NVENC does not apply it, and a PPS that signals it makes every residual decode wrong (T6.05).
#pragma once
#include <stddef.h>
#include <stdint.h>

typedef struct {
    int width, height;              // display size; coded size is whole 32x32 CTBs (conformance window crops it)
    int level_idc;                  // 30 x level (93 = 3.1, 123 = 4.1, 153 = 5.1); 0 = from the picture size at 60 fps
    int log2_max_poc_lsb_minus4;    // 4 (as nullmoth's)
    int max_dec_pic_buffering;      // DPB size in pictures (references + current)
    int num_reorder;                // sps_max_num_reorder_pics (B pictures: 1)
    int sps_rps_negative;           // pictures in the SPS's one short-term RPS (deltas 1, none used by the current picture)
    int num_ref_idx_l0_default;     // PPS default active references in list 0
    int pic_init_qp;                // 26 + init_qp_minus26
    int vui;                        // write VUI: square pixels, colour description
    int full_range;                 // VUI video_full_range_flag
    int colour[3];                  // VUI colour_primaries, transfer_characteristics, matrix_coefficients (H.273); 0 = BT.709 (1)
    int bit_depth;                  // 8 (Main) or 10 (Main 10); 0 = 8
    int chroma444;                  // 4:4:4 (Main 4:4:4 / Main 4:4:4 10, Range Extensions profile)
    int lossless;                   // transquant_bypass_enabled_flag (the engine bypasses every CU at QP 0)
    int min_cb8;                    // 8x8 coding blocks (min CB 8 instead of 16; NVIDIA's p6 / p7)
    int no_sdh, no_ts, no_cqd, no_sao, no_amp, sis;   // experiments (T6.05): coding tools off (sis: strong intra smoothing on)
} HevcParams;

int hevc_level_for(int width, int height);   // smallest Main-tier level for this size at 60 fps
size_t hevc_coded_width(const HevcParams *p);
size_t hevc_coded_height(const HevcParams *p);
// Annex B (start code 00 00 00 01) NAL units; return bytes written into out (needs ~128 bytes)
size_t hevc_write_vps(const HevcParams *p, uint8_t *out);
size_t hevc_write_sps(const HevcParams *p, uint8_t *out);
size_t hevc_write_pps(const HevcParams *p, uint8_t *out);
