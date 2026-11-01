// hevcsetup.c: print an H.265 picture setup buffer (nvenc_h265_drv_pic_setup_s + the controls it points to) by field,
// from a binary dump (scripts/trace2blob.py) or one our encoder wrote. T6.03.
//   ./hevcsetup setup.bin [setup2.bin]      with two files, only the fields that differ
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "nvenc_drv.h"
#include "nvencdump.h"

static unsigned char A[32768], B[32768]; static int two;
static char line[256];
#define OUT(fmt, ...) do { snprintf(line, sizeof line, fmt, __VA_ARGS__); } while (0)
// print field f of struct pointer p (a, b): both values when comparing, only if they differ
#define F(sa, sb, f) do { long long va = (long long)(sa)->f, vb = two ? (long long)(sb)->f : va; \
    if (!two) printf("  %-44s %lld (0x%llx)\n", #f, va, (unsigned long long)va); \
    else if (va != vb) printf("  %-44s %lld -> %lld\n", #f, va, vb); } while (0)

static void surface(const char *n, nvenc_h265_surface_cfg_s *a, nvenc_h265_surface_cfg_s *b)
{
    printf("%s:\n", n);
    F(a, b, frame_width_minus1); F(a, b, frame_height_minus1); F(a, b, sfc_pitch); F(a, b, sfc_pitch_chroma); F(a, b, sfc_trans_mode);
    F(a, b, luma_top_frm_offset); F(a, b, luma_bot_offset); F(a, b, chroma_top_frm_offset); F(a, b, chroma_bot_offset);
    F(a, b, block_height); F(a, b, tiled_16x16); F(a, b, memory_mode); F(a, b, nv21_enable); F(a, b, input_bl_mode);
}

int main(int argc, char **argv)
{
    if (argc < 2) { fprintf(stderr, "usage: hevcsetup setup.bin [setup2.bin]\n"); return 2; }
    FILE *f = fopen(argv[1], "rb"); if (!f || !fread(A, 1, sizeof A, f)) { perror(argv[1]); return 1; } fclose(f);
    if (argc > 2) { f = fopen(argv[2], "rb"); if (!f || !fread(B, 1, sizeof B, f)) { perror(argv[2]); return 1; } fclose(f); two = 1; }
    else memcpy(B, A, sizeof A);
    nvenc_h265_drv_pic_setup_s *a = (void *)A, *b = (void *)B;
    printf("magic 0x%08x%s\n", a->magic, two && a->magic != b->magic ? " (differs)" : "");
    surface("refpic_cfg", &a->refpic_cfg, &b->refpic_cfg);
    surface("input_cfg", &a->input_cfg, &b->input_cfg);
    surface("outputpic_cfg", &a->outputpic_cfg, &b->outputpic_cfg);
    printf("sps_data:\n");
    nvenc_h265_sps_data_s *s = &a->sps_data, *t = &b->sps_data;
    F(s, t, chroma_format_idc); F(s, t, separate_colour_plane_flag); F(s, t, log2_max_pic_order_cnt_lsb_minus4); F(s, t, min_cu_size);
    F(s, t, max_cu_size); F(s, t, min_tu_size); F(s, t, max_tu_size); F(s, t, max_transform_depth_inter); F(s, t, max_transform_depth_intra);
    F(s, t, pcm_enabled_flag); F(s, t, pcm_loop_filter_disable_flag); F(s, t, long_term_ref_pics_present_flag); F(s, t, amp_enabled_flag);
    F(s, t, strong_intra_smoothing_enabled_flag); F(s, t, num_short_term_ref_pic_sets); F(s, t, num_long_term_ref_pics_sps);
    F(s, t, bit_depth_minus_8); F(s, t, sample_adaptive_offset_enabled_flag); F(s, t, sps_temporal_mvp_enabled_flag);
    printf("pps_data:\n");
    nvenc_h265_pps_data_s *p = &a->pps_data, *q = &b->pps_data;
    F(p, q, pic_param_set_id); F(p, q, num_ref_idx_l0_active_minus1); F(p, q, num_ref_idx_l1_active_minus1); F(p, q, constrained_intra_pred_flag);
    F(p, q, transquant_bypass_enabled_flag); F(p, q, transform_skip_enabled_flag); F(p, q, deblocking_filter_override_enabled_flag);
    F(p, q, pps_loop_filter_across_slices_enabled_flag); F(p, q, cu_qp_delta_enabled_flag); F(p, q, lists_modification_present_flag);
    F(p, q, output_flag_present_flag); F(p, q, cabac_init_present_flag); F(p, q, pps_slice_chroma_qp_offsets_present); F(p, q, weighted_pred_flag);
    F(p, q, weighted_bipred_flag); F(p, q, pic_init_qp_minus26); F(p, q, pps_cb_qp_offset); F(p, q, pps_cr_qp_offset);
    F(p, q, dependent_slice_segments_enabled_flag); F(p, q, sign_data_hiding_enabled_flag); F(p, q, diff_cu_qp_delta_depth);
    F(p, q, log2_parallel_merge_level_minus2); F(p, q, num_extra_slice_header_bits);
    printf("rate_control:\n");
    nvenc_h265_rc_s *r = &a->rate_control, *u = &b->rate_control;
    F(r, u, hrd_type); F(r, u, QP[0]); F(r, u, QP[1]); F(r, u, QP[2]); F(r, u, minQP[0]); F(r, u, minQP[2]); F(r, u, maxQP[0]); F(r, u, maxQP[2]);
    F(r, u, maxQPD); F(r, u, baseQPD); F(r, u, rhopbi[0]); F(r, u, rhopbi[1]); F(r, u, rhopbi[2]); F(r, u, framerate); F(r, u, buffersize);
    F(r, u, nal_cpb_size); F(r, u, nal_bitrate); F(r, u, vcl_cpb_size); F(r, u, vcl_bitrate); F(r, u, gop_length); F(r, u, Np); F(r, u, Bmin);
    F(r, u, Ravg); F(r, u, R); F(r, u, ab_alpha); F(r, u, ab_beta); F(r, u, prev_act); F(r, u, single_frame_vbv); F(r, u, two_pass_rc);
    F(r, u, rc_class); F(r, u, i_size_ratio_x); F(r, u, i_size_ratio_y); F(r, u, aqMode); F(r, u, session_max_qp);
    printf("pic_control:\n");
    nvenc_h265_pic_control_s *c = &a->pic_control, *d = &b->pic_control;
    for (int i = 0; i < 8; i++) { F(c, d, l0[i]); } for (int i = 0; i < 2; i++) { F(c, d, temp_dist_l0[i]); }
    F(c, d, dist_scale_factor[0][0]); F(c, d, diff_pic_order_cnt_zero[0]); F(c, d, max_slice_size); F(c, d, max_byte_count_before_resid_zero);
    F(c, d, no_output_of_prior_pics_flag); F(c, d, pic_output_flag); F(c, d, mode_decision_only_flag); F(c, d, cu_early_terminatiom_enable);
    F(c, d, input_sub_sampling); F(c, d, output_sub_sampling); F(c, d, pic_order_cnt_lsb);
    F(c, d, num_forced_slices_minus1); F(c, d, num_me_controls_minus1); F(c, d, num_md_controls_minus1); F(c, d, num_q_controls_minus1);
    F(c, d, slice_control_offset); F(c, d, me_control_offset); F(c, d, md_control_offset); F(c, d, q_control_offset);
    F(c, d, hist_buf_size); F(c, d, bitstream_buf_size); F(c, d, bitstream_start_pos); F(c, d, pic_type); F(c, d, ref_pic_flag);
    F(c, d, slice_mode); F(c, d, cabac_zero_word_enable); F(c, d, codec); F(c, d, ceil_log2_pic_size_in_ctbs); F(c, d, sao_luma_mode);
    F(c, d, sao_chroma_mode); F(c, d, mpec_threshold); F(c, d, slice_stat_offset); F(c, d, mpec_stat_offset); F(c, d, aq_stat_offset);
    F(c, d, short_term_ref_pic_set_sps_flag); F(c, d, short_term_ref_pic_set_idx); F(c, d, e4byteStartCode); F(c, d, nal_unit_type);
    F(c, d, nuh_layer_id); F(c, d, nuh_temporal_id_plus1); F(c, d, qpfifo); F(c, d, enable_source_image_padding); F(c, d, luma_padding_mode);
    F(c, d, chroma_padding_mode); F(c, d, stats_fifo_enable);
    nvenc_h265_strps_s *g = &c->short_term_rps, *h = &d->short_term_rps;
    F(g, h, inter_ref_pic_set_prediction_flag); F(g, h, num_negative_pics); F(g, h, num_positive_pics); F(g, h, delta_poc_s0_minus1[0]);
    F(g, h, delta_poc_s0_minus1[1]); F(g, h, used_by_curr_pic_s0_flag); F(g, h, used_by_curr_pic_flag); F(g, h, use_delta_flag);
    F(c, d, rc_pass2_offset); F(c, d, wp_control_offset); F(c, d, num_wp_controls_minus1); F(c, d, stripID); F(c, d, strips_in_frame);
    F(c, d, slice_idx_start); F(c, d, slice_encoding_row_start); F(c, d, slice_encoding_row_num); F(c, d, chroma_skip_threshold_4x4);
    F(c, d, longTermFlag); F(c, d, poc_entry_l0[0]); F(c, d, poc_entry_l0[1]); F(c, d, same_poc[0]); F(c, d, act_stat_offset); F(c, d, stats_fifo_offset);
    F(a, b, gpTimer_timeout_val);
    // the controls (offsets from the start of the setup), raw dwords
    struct { const char *n; unsigned off, size; } ctl[] = { { "me_control", c->me_control_offset, sizeof(nvenc_h264_me_control_s) },
        { "md_control", c->md_control_offset, sizeof(nvenc_h265_md_control_s) }, { "q_control", c->q_control_offset, sizeof(nvenc_h265_quant_control_s) },
        { "slice_control", c->slice_control_offset, sizeof(nvenc_h265_slice_control_s) } };
    if (getenv("RAW") == NULL) {   // by field (scripts/structdump.py)
        if (c->me_control_offset) { printf("me_control @0x%x:\n", c->me_control_offset); dump_nvenc_h264_me_control_s((void *)(A + c->me_control_offset), two ? (void *)(B + d->me_control_offset) : NULL, ""); }
        if (c->md_control_offset) { printf("md_control @0x%x:\n", c->md_control_offset); dump_nvenc_h265_md_control_s((void *)(A + c->md_control_offset), two ? (void *)(B + d->md_control_offset) : NULL, ""); }
        if (c->q_control_offset) { printf("q_control @0x%x:\n", c->q_control_offset); dump_nvenc_h265_quant_control_s((void *)(A + c->q_control_offset), two ? (void *)(B + d->q_control_offset) : NULL, ""); }
        if (c->slice_control_offset) { printf("slice_control @0x%x:\n", c->slice_control_offset); dump_nvenc_h265_slice_control_s((void *)(A + c->slice_control_offset), two ? (void *)(B + d->slice_control_offset) : NULL, ""); }
        return 0;
    }
    for (int k = 0; k < 4; k++) {
        if (!ctl[k].off || ctl[k].off + ctl[k].size > sizeof A) { printf("%s: none\n", ctl[k].n); continue; }
        printf("%s @0x%x (%u B):", ctl[k].n, ctl[k].off, ctl[k].size);
        const unsigned *wa = (const unsigned *)(A + ctl[k].off), *wb = (const unsigned *)(B + ctl[k].off); int any = 0;
        for (unsigned i = 0; i < ctl[k].size / 4; i++) {
            if (!two) { if (i % 8 == 0) printf("\n    %03x:", i * 4); printf(" %08x", wa[i]); }
            else if (wa[i] != wb[i]) { printf("\n    +%03x: %08x -> %08x", i * 4, wa[i], wb[i]); any = 1; }
        }
        printf(two && !any ? " same\n" : "\n");
    }
    return 0;
}
