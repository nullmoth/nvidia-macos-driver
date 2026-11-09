// hevchdr.c: see hevchdr.h. Exp-Golomb bit writer + emulation prevention (as h264hdr.c).
#include "hevchdr.h"
#include <stdbool.h>
#include <string.h>

typedef struct { uint8_t buf[192]; int bits; } Bw;
static void put(Bw *b, unsigned v, int n) { for (int i = n - 1; i >= 0; i--, b->bits++) if ((v >> i) & 1) b->buf[b->bits >> 3] |= (uint8_t)(0x80 >> (b->bits & 7)); }
static void ue(Bw *b, unsigned v) { unsigned x = v + 1; int n = 0; while ((x >> n) > 1) n++; put(b, 0, n); put(b, x, n + 1); }
static void se(Bw *b, int v) { ue(b, v > 0 ? 2u * (unsigned)v - 1 : 2u * (unsigned)(-v)); }
static void trailing(Bw *b) { put(b, 1, 1); while (b->bits & 7) put(b, 0, 1); }

static size_t emit(const Bw *b, unsigned nal_type, uint8_t *out)
{
    size_t n = 0, len = (size_t)(b->bits >> 3); int zeros = 0;
    out[n++] = 0; out[n++] = 0; out[n++] = 0; out[n++] = 1;
    out[n++] = (uint8_t)(nal_type << 1); out[n++] = 1;          // forbidden 0, type, nuh_layer_id 0, temporal_id_plus1 1
    for (size_t i = 0; i < len; i++) {
        if (zeros >= 2 && b->buf[i] <= 3) { out[n++] = 3; zeros = 0; }   // emulation prevention
        out[n++] = b->buf[i]; zeros = b->buf[i] ? 0 : zeros + 1;
    }
    return n;
}

int hevc_level_for(int width, int height)
{
    long ps = (long)width * height, sr = ps * 60;               // A.4.1: MaxLumaPs, MaxLumaSr (Main tier)
    if (ps <= 552960 && sr <= 33177600) return 93;              // 3.1
    if (ps <= 2228224 && sr <= 133693440) return 123;           // 4.1
    if (ps <= 8912896 && sr <= 534773760) return 153;           // 5.1
    return 183;                                                 // 6.1
}
static int level(const HevcParams *p) { return p->level_idc ? p->level_idc : hevc_level_for(p->width, p->height); }
// whole 32x32 CTBs: NVENC encodes a partial last CTB row/column as if it were whole (a 720-line picture coded as 720 lines
// decoded wrong), so the picture is coded CTB-aligned and the conformance window crops it, as nullmoth's does (T6.06)
size_t hevc_coded_width(const HevcParams *p) { return ((size_t)p->width + 31) & ~(size_t)31; }
size_t hevc_coded_height(const HevcParams *p) { return ((size_t)p->height + 31) & ~(size_t)31; }

static void profile_tier_level(Bw *b, int level_idc, bool ten, bool rext)  // 7.3.3, sps_max_sub_layers_minus1 = 0
{
    put(b, 0, 2); put(b, 0, 1); put(b, rext ? 4 : ten ? 2 : 1, 5);   // general_profile_space, tier Main, profile Main / Main 10 / RExt
    put(b, rext ? 0x08000000u : ten ? 0x20000000u : 0x60000000u, 32);   // compatibility flags: RExt / Main 10 only / Main and Main 10
    put(b, 1, 1); put(b, 0, 1); put(b, 0, 1); put(b, 1, 1);     // progressive, not interlaced, not non-packed, frame only
    if (rext) {   // Main 4:4:4 (10): max_12bit, max_10bit, max_8bit (8-bit only), no 4:2:2 / 4:2:0 / monochrome / intra / one picture limits, lower_bit_rate
        put(b, 1, 1); put(b, 1, 1); put(b, ten ? 0 : 1, 1); put(b, 0, 1); put(b, 0, 1); put(b, 0, 1); put(b, 0, 1); put(b, 0, 1); put(b, 1, 1);
        put(b, 0, 32); put(b, 0, 2);                            // 34 reserved bits
    } else { put(b, 0, 32); put(b, 0, 11); }                    // 43 reserved constraint bits
    put(b, 0, 1);                                               // general_inbld_flag
    put(b, (unsigned)level_idc, 8);
}

size_t hevc_write_vps(const HevcParams *p, uint8_t *out)
{
    Bw b; memset(&b, 0, sizeof b);
    put(&b, 0, 4);                                              // vps_video_parameter_set_id
    put(&b, 1, 1); put(&b, 1, 1);                               // base layer internal / available
    put(&b, 0, 6); put(&b, 0, 3); put(&b, 1, 1);                // max_layers_minus1, max_sub_layers_minus1, temporal_id_nesting
    put(&b, 0xffff, 16);                                        // vps_reserved_0xffff_16bits
    profile_tier_level(&b, level(p), p->bit_depth > 8, p->chroma444);
    put(&b, 1, 1);                                              // vps_sub_layer_ordering_info_present_flag
    ue(&b, (unsigned)p->max_dec_pic_buffering - 1); ue(&b, (unsigned)p->num_reorder); ue(&b, 0);   // max_dec_pic_buffering_minus1, reorder, no latency limit
    put(&b, 0, 6); ue(&b, 0);                                   // vps_max_layer_id, vps_num_layer_sets_minus1
    put(&b, 0, 1);                                              // vps_timing_info_present_flag
    put(&b, 0, 1);                                              // vps_extension_flag
    trailing(&b);
    return emit(&b, 32, out);
}

size_t hevc_write_sps(const HevcParams *p, uint8_t *out)
{
    Bw b; memset(&b, 0, sizeof b);
    unsigned cw = (unsigned)hevc_coded_width(p), ch = (unsigned)hevc_coded_height(p);
    put(&b, 0, 4); put(&b, 0, 3); put(&b, 1, 1);                // vps id, max_sub_layers_minus1, temporal_id_nesting
    profile_tier_level(&b, level(p), p->bit_depth > 8, p->chroma444);
    ue(&b, 0);                                                  // sps_seq_parameter_set_id
    ue(&b, p->chroma444 ? 3 : 1);                               // chroma_format_idc 4:4:4 / 4:2:0
    if (p->chroma444) put(&b, 0, 1);                            // separate_colour_plane_flag
    ue(&b, cw); ue(&b, ch);
    const unsigned u = p->chroma444 ? 1 : 2;                    // conformance window units: SubWidthC / SubHeightC
    unsigned cr = (cw - (unsigned)p->width) / u, cb = (ch - (unsigned)p->height) / u;
    put(&b, cr || cb ? 1 : 0, 1);
    if (cr || cb) { ue(&b, 0); ue(&b, cr); ue(&b, 0); ue(&b, cb); }
    unsigned bd = p->bit_depth > 8 ? (unsigned)p->bit_depth - 8 : 0;
    ue(&b, bd); ue(&b, bd);                                     // bit depth luma / chroma - 8
    ue(&b, (unsigned)p->log2_max_poc_lsb_minus4);
    put(&b, 1, 1);                                              // sps_sub_layer_ordering_info_present_flag
    ue(&b, (unsigned)p->max_dec_pic_buffering - 1); ue(&b, (unsigned)p->num_reorder); ue(&b, 0);
    ue(&b, p->min_cb8 ? 0 : 1); ue(&b, p->min_cb8 ? 2 : 1);     // min CB 16 (log2 - 3 = 1) or 8, max CB 32
    ue(&b, 0); ue(&b, 3);                                       // min TB 4 (log2 - 2 = 0), max TB 32 (diff 3)
    ue(&b, 3); ue(&b, 3);                                       // max_transform_hierarchy_depth inter / intra
    put(&b, 0, 1);                                              // scaling_list_enabled_flag
    put(&b, p->no_amp ? 0 : 1, 1); put(&b, p->no_sao ? 0 : 1, 1); put(&b, 0, 1);   // amp, sample_adaptive_offset, pcm
    ue(&b, 1);                                                  // num_short_term_ref_pic_sets
    ue(&b, (unsigned)p->sps_rps_negative); ue(&b, 0);           // st_ref_pic_set(0): negative pictures, no positive
    for (int i = 0; i < p->sps_rps_negative; i++) { ue(&b, 0); put(&b, 0, 1); }   // delta_poc_s0_minus1 0, not used by curr
    put(&b, 0, 1);                                              // long_term_ref_pics_present_flag
    put(&b, 0, 1); put(&b, p->sis ? 1 : 0, 1);                  // sps_temporal_mvp_enabled_flag, strong_intra_smoothing_enabled_flag
    put(&b, p->vui ? 1 : 0, 1);
    if (p->vui) {                                               // E.2.1
        put(&b, 1, 1); put(&b, 1, 8);                           // aspect_ratio_info_present, aspect_ratio_idc 1 (square)
        put(&b, 0, 1);                                          // overscan_info_present
        put(&b, 1, 1); put(&b, 5, 3); put(&b, (unsigned)p->full_range, 1);   // video_signal_type: format unspecified, full range flag
        put(&b, 1, 1); for (int i = 0; i < 3; i++) put(&b, (unsigned)(p->colour[i] ? p->colour[i] : 1), 8);   // colour description
        put(&b, 0, 1);                                          // chroma_loc_info_present
        put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 1);            // neutral_chroma, field_seq, frame_field_info
        put(&b, 0, 1);                                          // default_display_window_flag
        put(&b, 0, 1);                                          // vui_timing_info_present_flag
        put(&b, 0, 1);                                          // bitstream_restriction_flag
    }
    put(&b, 0, 1);                                              // sps_extension_present_flag
    trailing(&b);
    return emit(&b, 33, out);
}

size_t hevc_write_pps(const HevcParams *p, uint8_t *out)
{
    Bw b; memset(&b, 0, sizeof b);
    ue(&b, 0); ue(&b, 0);                                       // pps id, sps id
    put(&b, 0, 1); put(&b, 0, 1); put(&b, 0, 3);                // dependent slices, output_flag_present, extra slice header bits
    put(&b, p->no_sdh ? 0 : 1, 1); put(&b, 1, 1);               // sign_data_hiding, cabac_init_present
    ue(&b, (unsigned)(p->num_ref_idx_l0_default ? p->num_ref_idx_l0_default : 1) - 1); ue(&b, 0);   // l0 / l1 default active - 1
    se(&b, p->pic_init_qp - 26);
    put(&b, 0, 1); put(&b, p->no_ts ? 0 : 1, 1);                // constrained_intra_pred, transform_skip_enabled
    put(&b, p->no_cqd ? 0 : 1, 1); if (!p->no_cqd) ue(&b, 0);   // cu_qp_delta_enabled, diff_cu_qp_delta_depth 0
    se(&b, 0); se(&b, 0); put(&b, 0, 1);                        // cb / cr qp offsets, slice chroma offsets present
    put(&b, 0, 1); put(&b, 0, 1); put(&b, p->lossless ? 1 : 0, 1);   // weighted pred / bipred, transquant bypass
    put(&b, 0, 1); put(&b, 0, 1);                               // tiles, entropy coding sync
    put(&b, 1, 1);                                              // pps_loop_filter_across_slices_enabled_flag
    put(&b, 1, 1); put(&b, 0, 1); put(&b, 0, 1); se(&b, 0); se(&b, 0);   // deblocking control present: no override, enabled, offsets 0
    put(&b, 0, 1); put(&b, 0, 1);                               // pps scaling list, lists_modification_present
    ue(&b, 0); put(&b, 0, 1); put(&b, 0, 1);                    // log2_parallel_merge_level - 2, slice header extension, pps extension
    trailing(&b);
    return emit(&b, 34, out);
}
