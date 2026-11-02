// av1setup-dump.c (M12): print a traced NVENC AV1 picture setup (Ada) field by field. Ada's layout = nvenc_drv.h's
// nvenc_av1_drv_pic_setup_s with one more 32-byte surface config after outputpic_cfg (0x64): it is removed here.
//   cc -I ../vendor/open-gpu-doc/classes/video -o /tmp/av1setup-dump scripts/av1setup-dump.c && /tmp/av1setup-dump setup.bin
#include <stdio.h>
#include <string.h>
#include <stddef.h>
#define NV_NVENC_8_2 1
#include "nvenc_drv.h"
#define F(s, f) printf("  %-34s %d\n", #f, (int)(s)->f)
static void surf(const char *n, const nvenc_av1_surface_cfg_s *c)
{
    printf("%s: %ux%u pitch %u/%u trans %u luma_top 0x%x luma_bot 0x%x chroma_top 0x%x chroma_bot 0x%x block_height %u tiled16 %u mem_mode %u nv21 %u bl_mode %u lsb %u\n", n,
           c->frame_width_minus1 + 1, c->frame_height_minus1 + 1, c->sfc_pitch, c->sfc_pitch_chroma, c->sfc_trans_mode, c->luma_top_frm_offset,
           c->luma_bot_offset, c->chroma_top_frm_offset, c->chroma_bot_offset, c->block_height, c->tiled_16x16, c->memory_mode, c->nv21_enable,
           c->input_bl_mode, c->src_in_lsb);
}
int main(int argc, char **argv)
{
    static unsigned char b[0x2000]; FILE *f = fopen(argv[1], "rb"); size_t n = fread(b, 1, sizeof b, f); fclose(f); (void)n;
    nvenc_av1_drv_pic_setup_s ps; memcpy(&ps, b, 0x64); memcpy((char *)&ps + 0x64, b + 0x84, sizeof ps - 0x64);
    printf("magic 0x%08x\n", ps.magic);
    surf("refpic", &ps.refpic_cfg); surf("input", &ps.input_cfg); surf("output", &ps.outputpic_cfg); surf("extra@0x64", (const void *)(b + 0x64));
    surf("half_scaled", &ps.half_scaled_outputpic_cfg);
    const nvenc_av1_seq_hdr_data_s *s = &ps.seqhdr_data; printf("seqhdr:\n");
    F(s, min_cu_size); F(s, max_cu_size); F(s, chroma_format_idc); F(s, monochrome_flag); F(s, bit_depth_minus_8); F(s, input_bit_depth_minus8);
    F(s, enable_filter_intra); F(s, enable_intra_edge_filter); F(s, enable_interintra_compound); F(s, enable_masked_compound); F(s, enable_dual_filter);
    F(s, force_screen_content_tools); F(s, force_integer_mv); F(s, enable_jnt_comp); F(s, enable_ref_frame_mvs); F(s, enable_warped_motion);
    F(s, enable_superres); F(s, enable_cdef); F(s, enable_restoration);
    const nvenc_av1_frame_hdr_data_s *h = &ps.framehdr_data; printf("framehdr:\n");
    F(h, frame_type); F(h, disable_cdf_update); F(h, allow_screen_content_tools); F(h, force_integer_mv); F(h, allow_high_precision_mv);
    F(h, switchable_motion_mode); F(h, is_filter_switchable); F(h, use_ref_frame_mvs); F(h, frame_end_update_cdf); F(h, interp_filter);
    F(h, delta_q_present_flag); F(h, delta_q_res); F(h, allow_intrabc); F(h, delta_lf_present_flag); F(h, delta_lf_res); F(h, delta_lf_multi);
    F(h, using_qmatrix); F(h, frame_tx_mode); F(h, frame_reference_mode); F(h, skip_mode_flag); F(h, allow_warped_motion); F(h, reduced_tx_set_flag);
    F(h, coded_lossless); F(h, ssim_rdo_enable); F(h, ssim_rdo_alt_frame_scale); F(h, bicompound_enable); F(h, unicompound_enable);
    F(h, mc_hfilter); F(h, mc_vfilter); F(h, segmentation_enable); F(h, segmentation_update_map); F(h, segment_temporal_update);
    F(h, segIdPreSkip); F(h, LastActiveSegId);
    printf("  ref_frame_sign_bias %d %d %d %d %d %d %d %d\n", h->ref_frame_sign_bias[0], h->ref_frame_sign_bias[1], h->ref_frame_sign_bias[2],
           h->ref_frame_sign_bias[3], h->ref_frame_sign_bias[4], h->ref_frame_sign_bias[5], h->ref_frame_sign_bias[6], h->ref_frame_sign_bias[7]);
    const nvenc_av1_loop_filter_cfg_s *lf = &h->lf_cfg;
    printf("  lf: level %u %u %u %u ref_delta %d %d %d %d %d %d %d %d mode %d %d upd_ref 0x%x upd_mode %u %u delta_enabled %u sharpness %u\n",
           lf->filter_level[0], lf->filter_level[1], lf->filter_level[2], lf->filter_level[3], lf->ref_delta[0], lf->ref_delta[1], lf->ref_delta[2],
           lf->ref_delta[3], lf->ref_delta[4], lf->ref_delta[5], lf->ref_delta[6], lf->ref_delta[7], lf->mode_delta_0, lf->mode_delta_1,
           lf->update_ref_delta_flag, lf->update_mode_delta0, lf->update_mode_delta1, lf->delta_enabled, lf->sharpness);
    const nvenc_av1_cdef_cfg_s *c = &h->cdef_cfg;
    printf("  cdef: damping-3 %u bits %u y %u %u %u %u uv %u %u %u %u y_num-1 %u uv_num-1 %u comb-1 %u comb_luma 0x%x comb_chroma 0x%x\n", c->cdef_damping_minus_3,
           c->cdef_bits, c->y_strength_0, c->y_strength_1, c->y_strength_2, c->y_strength_3, c->uv_strength_0, c->uv_strength_1, c->uv_strength_2,
           c->uv_strength_3, c->cdef_y_strength_num_minus_1, c->cdef_uv_strength_num_minus_1, c->cdef_combined_idx_minus_1, c->cdef_combined_luma_idx,
           c->cdef_combined_chroma_idx);
    printf("  quant: base_q_idx %u dc_y %d dc_uv %d ac_uv %d\n", h->quant_cfg.base_q_idx, h->quant_cfg.delta_y_dc, h->quant_cfg.delta_uv_dc, h->quant_cfg.delta_uv_ac);
    const nvenc_av1_lr_filter_cfg_s *lr = &h->lrf_cfg;
    printf("  lr: y %u uv %u y_unit %u uv_unit %u codebooks 0x%x pe_on_edge %u\n", lr->y_lr_type, lr->uv_lr_type, lr->y_lr_unit_size, lr->uv_lr_unit_size,
           lr->lr_codebook_enable, lr->lr_pe_on_edge_enable);
    const nvenc_av1_rc_s *r = &ps.rate_control; printf("rc:\n");
    printf("  hrd %u QP %u %u %u min %u %u %u max %u %u %u maxQPD %d baseQPD %d rhopbi %u %u %u fr %u buf %u nal %u/%u vcl %u/%u gop %u Np %u Bmin %u Ravg %u R %u\n",
           r->hrd_type, r->QP[0], r->QP[1], r->QP[2], r->minQP[0], r->minQP[1], r->minQP[2], r->maxQP[0], r->maxQP[1], r->maxQP[2], r->maxQPD, r->baseQPD,
           r->rhopbi[0], r->rhopbi[1], r->rhopbi[2], r->framerate, r->buffersize, r->nal_cpb_size, r->nal_bitrate, r->vcl_cpb_size, r->vcl_bitrate,
           r->gop_length, r->Np, r->Bmin, r->Ravg, r->R);
    printf("  ab %u %u prev_act %u sfv %u two_pass %u class %u fp_half %u isr %u/%u aq %u dump_aq %u\n", r->ab_alpha, r->ab_beta, r->prev_act, r->single_frame_vbv,
           r->two_pass_rc, r->rc_class, r->first_pass_source_half_scaled, r->i_size_ratio_x, r->i_size_ratio_y, r->aqMode, r->dump_aq_stats);
    const nvenc_av1_pic_control_s *p = &ps.pic_control; printf("pic_control:\n");
    printf("  l0 %d %d %d %d l1 %d %d %d td_l0 %d %d %d %d td_l1 %d %d %d poc_l0 %d %d %d %d poc_l1 %d %d %d\n", p->l0[0], p->l0[1], p->l0[2], p->l0[3],
           p->l1[0], p->l1[1], p->l1[2], p->temp_dist_l0[0], p->temp_dist_l0[1], p->temp_dist_l0[2], p->temp_dist_l0[3], p->temp_dist_l1[0], p->temp_dist_l1[1],
           p->temp_dist_l1[2], p->poc_entry_l0[0], p->poc_entry_l0[1], p->poc_entry_l0[2], p->poc_entry_l0[3], p->poc_entry_l1[0], p->poc_entry_l1[1], p->poc_entry_l1[2]);
    F(p, max_byte_count_before_resid_zero); F(p, mode_decision_only_flag); F(p, cu_early_terminatiom_enable); F(p, pic_output_flag);
    F(p, input_sub_sampling); F(p, output_sub_sampling); F(p, num_ref_idx_l0_active); F(p, num_ref_idx_l1_active); F(p, tile_idx_start);
    F(p, num_strip_tiles_minus1); F(p, force_lf_on_flag); F(p, force_lf_off_flag); F(p, max_tu_depth_inter); F(p, max_tu_depth_intra);
    F(p, num_tiles_minus1); F(p, enable_source_image_padding); F(p, luma_padding_mode); F(p, chroma_padding_mode); F(p, prob_tables_offset);
    F(p, tile_control_offset); F(p, hist_buf_size); F(p, bitstream_buf_size); F(p, bitstream_start_pos); F(p, pic_type); F(p, ref_pic_flag);
    F(p, codec); F(p, qpfifo); F(p, num_tile_cols); F(p, num_tile_rows); F(p, ctx_update_tile_id); F(p, stats_fifo_enable); F(p, mpec_threshold);
    F(p, tile_stat_offset); F(p, mpec_stat_offset); F(p, aq_stat_offset); F(p, rc_pass2_offset); F(p, act_stat_offset); F(p, external_hint_number);
    F(p, stripID); F(p, strips_in_frame); F(p, strip_width_in_sbs); F(p, strip_height_in_sbs); F(p, strip_width_offset_in_sbs);
    F(p, strip_height_offset_in_sbs); F(p, stats_fifo_offset);
    printf("md_control (hex words):"); for (int i = 0; i < 40; i++) printf("%s%08x", i % 8 ? " " : "\n  ", ((const unsigned *)&ps.md_control)[i]); printf("\n");
    printf("me_control (hex words):"); for (int i = 0; i < 48; i++) printf("%s%08x", i % 8 ? " " : "\n  ", ((const unsigned *)&ps.me_control)[i]); printf("\n");
    printf("gpTimer_timeout_val %u\n", ps.gpTimer_timeout_val);
    if (p->tile_control_offset && p->tile_control_offset + 32 <= sizeof b) {
        const nvenc_av1_tile_control_s *t = (const void *)(b + p->tile_control_offset);
        printf("tile control @0x%x: top %u,%u bot %u,%u qp %u..%u avr %u type %u force_intra %u end_upd %u limits %u%u%u%u idx %u disf %u%u%u%u next %u,%u..%u,%u ir %u/%u\n",
               p->tile_control_offset, t->tile_top_sbx, t->tile_top_sby, t->tile_bot_sbx, t->tile_bot_sby, t->qp_tile_min, t->qp_tile_max, t->qp_avr,
               t->tile_type, t->force_intra, t->tile_end_update_cdf, t->limit_tile_top_boundary, t->limit_tile_bot_boundary, t->limit_tile_left_boundary,
               t->limit_tile_right_boundary, t->tile_idx, t->disable_tile_top_filters, t->disable_tile_bottom_filters, t->disable_next_tile_top_filters,
               t->disable_next_tile_bottom_filters, t->next_tile_top_sbx, t->next_tile_top_sby, t->next_tile_bot_sbx, t->next_tile_bot_sby,
               t->intra_refresh_cnt, t->intra_refresh_offs);
    }
    return 0;
}
