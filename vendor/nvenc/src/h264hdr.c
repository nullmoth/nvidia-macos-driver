// h264hdr.c: see h264hdr.h. Exp-Golomb bit writer + emulation prevention.
#include "h264hdr.h"
#include <string.h>

typedef struct { uint8_t buf[128]; int bits; } Bw;
static void put(Bw *b, unsigned v, int n) { for (int i = n - 1; i >= 0; i--, b->bits++) if ((v >> i) & 1) b->buf[b->bits >> 3] |= (uint8_t)(0x80 >> (b->bits & 7)); }
static void ue(Bw *b, unsigned v) { unsigned x = v + 1; int n = 0; while ((x >> n) > 1) n++; put(b, 0, n); put(b, x, n + 1); }
static void se(Bw *b, int v) { ue(b, v > 0 ? 2u * (unsigned)v - 1 : 2u * (unsigned)(-v)); }
static void trailing(Bw *b) { put(b, 1, 1); while (b->bits & 7) put(b, 0, 1); }

static size_t emit(const Bw *b, uint8_t nal_header, uint8_t *out)
{
    size_t n = 0, len = (size_t)(b->bits >> 3); int zeros = 0;
    out[n++] = 0; out[n++] = 0; out[n++] = 0; out[n++] = 1; out[n++] = nal_header;
    for (size_t i = 0; i < len; i++) {
        if (zeros >= 2 && b->buf[i] <= 3) { out[n++] = 3; zeros = 0; }   // emulation prevention
        out[n++] = b->buf[i]; zeros = b->buf[i] ? 0 : zeros + 1;
    }
    return n;
}

size_t h264_write_sps(const H264Params *p, uint8_t *out)
{
    Bw b; memset(&b, 0, sizeof b);
    int wmb = (p->width + 15) / 16, hmb = (p->height + 15) / 16;
    put(&b, (unsigned)p->profile_idc, 8);
    put(&b, 0, 8);                                     // constraint flags + reserved
    put(&b, (unsigned)p->level_idc, 8);
    ue(&b, 0);                                         // seq_parameter_set_id
    if (p->profile_idc >= 100) {
        ue(&b, p->chroma444 ? 3 : 1);                  // chroma_format_idc 4:4:4 / 4:2:0
        if (p->chroma444) put(&b, 0, 1);               // separate_colour_plane_flag
        ue(&b, 0); ue(&b, 0);                          // bit depth luma/chroma - 8
        put(&b, p->lossless ? 1 : 0, 1);               // qpprime_y_zero_transform_bypass
        put(&b, 0, 1);                                 // seq_scaling_matrix_present
    }
    ue(&b, (unsigned)p->log2_max_frame_num_minus4);
    ue(&b, (unsigned)p->poc_type);                     // pic_order_cnt_type 0 or 2
    if (p->poc_type == 0) ue(&b, (unsigned)p->log2_max_poc_lsb_minus4);
    ue(&b, (unsigned)p->num_ref_frames);
    put(&b, 0, 1);                                     // gaps_in_frame_num_value_allowed
    ue(&b, (unsigned)wmb - 1);
    ue(&b, (unsigned)hmb - 1);                         // frame_mbs_only: map units = macroblocks
    put(&b, 1, 1);                                     // frame_mbs_only_flag
    put(&b, 1, 1);                                     // direct_8x8_inference
    const int cu = p->chroma444 ? 1 : 2;               // crop units: 2 pixels in 4:2:0, 1 in 4:4:4
    int cr = (wmb * 16 - p->width) / cu, cb = (hmb * 16 - p->height) / cu;
    put(&b, cr || cb ? 1 : 0, 1);
    if (cr || cb) { ue(&b, 0); ue(&b, (unsigned)cr); ue(&b, 0); ue(&b, (unsigned)cb); }
    put(&b, p->vui ? 1 : 0, 1);                        // vui_parameters_present
    if (p->vui) {                                      // E.1.1
        put(&b, 0, 1);                                 // aspect_ratio_info_present
        put(&b, 0, 1);                                 // overscan_info_present
        put(&b, 1, 1);                                 // video_signal_type_present
        put(&b, 5, 3); put(&b, (unsigned)p->full_range, 1);   // video_format unspecified, video_full_range_flag
        put(&b, 1, 1); for (int i = 0; i < 3; i++) put(&b, (unsigned)(p->colour[i] ? p->colour[i] : 1), 8);   // colour description
        put(&b, 0, 1);                                 // chroma_loc_info_present
        put(&b, 0, 1);                                 // timing_info_present
        put(&b, 0, 1); put(&b, 0, 1);                  // nal / vcl hrd_parameters_present
        put(&b, 0, 1);                                 // pic_struct_present
        put(&b, 1, 1);                                 // bitstream_restriction_flag
        put(&b, 1, 1);                                 // motion_vectors_over_pic_boundaries
        ue(&b, 2); ue(&b, 1);                          // max_bytes_per_pic_denom, max_bits_per_mb_denom (defaults)
        ue(&b, 16); ue(&b, 16);                        // log2_max_mv_length_horizontal / vertical
        ue(&b, (unsigned)p->num_reorder);              // max_num_reorder_frames (0: output in decode order, at once)
        ue(&b, (unsigned)p->num_ref_frames);           // max_dec_frame_buffering
    }
    trailing(&b);
    return emit(&b, 0x67, out);                        // nal_ref_idc 3, type 7
}

size_t h264_write_pps(const H264Params *p, uint8_t *out)
{
    Bw b; memset(&b, 0, sizeof b);
    ue(&b, 0); ue(&b, 0);                              // pic_parameter_set_id, seq_parameter_set_id
    put(&b, (unsigned)p->entropy_cabac, 1);
    put(&b, 0, 1);                                     // bottom_field_pic_order_in_frame_present
    ue(&b, 0);                                         // num_slice_groups_minus1
    ue(&b, (unsigned)p->num_ref_idx_l0_minus1); ue(&b, 0);   // num_ref_idx_l0/l1_default_active_minus1
    put(&b, 0, 1); put(&b, 0, 2);                      // weighted_pred, weighted_bipred_idc
    se(&b, p->pic_init_qp - 26);
    se(&b, 0);                                         // pic_init_qs_minus26
    se(&b, 0);                                         // chroma_qp_index_offset
    put(&b, 1, 1);                                     // deblocking_filter_control_present
    put(&b, 0, 1);                                     // constrained_intra_pred
    put(&b, 0, 1);                                     // redundant_pic_cnt_present
    if (p->transform_8x8) { put(&b, 1, 1); put(&b, 0, 1); se(&b, 0); }   // transform_8x8_mode, no scaling, 2nd chroma qp
    trailing(&b);
    return emit(&b, 0x68, out);
}

size_t hdr_write_sei(bool hevc, const uint8_t *mdcv, const uint8_t *cll, uint8_t *out)   // H.264 D.1.29 / D.1.31, H.265 D.2.28 / D.2.35
{
    uint8_t rbsp[64]; size_t r = 0;
    if (mdcv) { rbsp[r++] = 137; rbsp[r++] = 24; memcpy(rbsp + r, mdcv, 24); r += 24; }   // mastering_display_colour_volume
    if (cll) { rbsp[r++] = 144; rbsp[r++] = 4; memcpy(rbsp + r, cll, 4); r += 4; }         // content_light_level_info
    if (!r) return 0;
    rbsp[r++] = 0x80;                                          // rbsp_trailing_bits
    size_t n = 0; int zeros = 0;
    out[n++] = 0; out[n++] = 0; out[n++] = 0; out[n++] = 1;
    if (hevc) { out[n++] = 39 << 1; out[n++] = 1; } else out[n++] = 6;   // prefix SEI (layer 0, TemporalId 0) / SEI
    for (size_t i = 0; i < r; i++) {
        if (zeros >= 2 && rbsp[i] <= 3) { out[n++] = 3; zeros = 0; }   // emulation prevention
        out[n++] = rbsp[i]; zeros = rbsp[i] ? 0 : zeros + 1;
    }
    return n;
}
