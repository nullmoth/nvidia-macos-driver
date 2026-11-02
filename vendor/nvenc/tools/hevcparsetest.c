// hevcparsetest.c (M11, T11.01): parse every NAL unit of an Annex B HEVC file with src/hevcparse.c. With HEVCTRACE=1 prints
// the SPS / PPS / slice segment header fields as "name = value [@bit]" in ffmpeg trace_headers' names
// (scripts/hevc-trace-compare.py); else one line per picture with its POC and reference picture set.
//   ./hevcparsetest in.hevc
#include "hevcparse.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define P(n, v) printf("%s = %d\n", n, (int)(v))
static void trace_sps(const HevcSps *s)
{
    printf("SECTION SPS\n");
    P("sps_video_parameter_set_id", s->vps_id); P("sps_max_sub_layers_minus1", s->max_sub_layers_minus1);
    P("general_profile_idc", s->general_profile_idc); P("general_level_idc", s->general_level_idc);
    P("sps_seq_parameter_set_id", s->sps_id); P("chroma_format_idc", s->chroma_format_idc);
    if (s->chroma_format_idc == 3) P("separate_colour_plane_flag", s->separate_colour_plane_flag);
    P("pic_width_in_luma_samples", s->width); P("pic_height_in_luma_samples", s->height);
    const int sw = (s->chroma_format_idc == 1 || s->chroma_format_idc == 2) ? 2 : 1, sh = s->chroma_format_idc == 1 ? 2 : 1;
    P("conf_win_left_offset", s->conf_win_left / sw); P("conf_win_right_offset", s->conf_win_right / sw);
    P("conf_win_top_offset", s->conf_win_top / sh); P("conf_win_bottom_offset", s->conf_win_bottom / sh);
    P("bit_depth_luma_minus8", s->bit_depth_luma - 8); P("bit_depth_chroma_minus8", s->bit_depth_chroma - 8);
    P("log2_max_pic_order_cnt_lsb_minus4", s->log2_max_poc_lsb - 4);
    for (int i = 0; i <= s->max_sub_layers_minus1; i++) {
        printf("sps_max_dec_pic_buffering_minus1[%d] = %d\nsps_max_num_reorder_pics[%d] = %d\n", i, s->max_dec_pic_buffering[i] - 1, i, s->max_num_reorder[i]);
    }
    P("log2_min_luma_coding_block_size_minus3", s->log2_min_cb - 3); P("log2_diff_max_min_luma_coding_block_size", s->log2_max_cb - s->log2_min_cb);
    P("log2_min_luma_transform_block_size_minus2", s->log2_min_tb - 2); P("log2_diff_max_min_luma_transform_block_size", s->log2_max_tb - s->log2_min_tb);
    P("max_transform_hierarchy_depth_inter", s->max_th_depth_inter); P("max_transform_hierarchy_depth_intra", s->max_th_depth_intra);
    P("scaling_list_enabled_flag", s->scaling_list_enabled); P("sps_scaling_list_data_present_flag", s->scaling_list_data_present);
    P("amp_enabled_flag", s->amp_enabled); P("sample_adaptive_offset_enabled_flag", s->sao_enabled); P("pcm_enabled_flag", s->pcm_enabled);
    if (s->pcm_enabled) {
        P("pcm_sample_bit_depth_luma_minus1", s->pcm_bit_depth_luma - 1); P("pcm_sample_bit_depth_chroma_minus1", s->pcm_bit_depth_chroma - 1);
        P("log2_min_pcm_luma_coding_block_size_minus3", s->log2_min_pcm_cb - 3);
        P("log2_diff_max_min_pcm_luma_coding_block_size", s->log2_max_pcm_cb - s->log2_min_pcm_cb); P("pcm_loop_filter_disabled_flag", s->pcm_loop_filter_disabled);
    }
    P("num_short_term_ref_pic_sets", s->num_short_term_ref_pic_sets); P("long_term_ref_pics_present_flag", s->long_term_ref_pics_present);
    if (s->long_term_ref_pics_present) P("num_long_term_ref_pics_sps", s->num_long_term_ref_pics_sps);
    P("sps_temporal_mvp_enabled_flag", s->temporal_mvp_enabled); P("strong_intra_smoothing_enabled_flag", s->strong_intra_smoothing_enabled);
    P("vui_parameters_present_flag", s->vui_present);
    if (s->vui_present) P("video_full_range_flag", s->video_full_range_flag);
    if (s->end_bit) printf("rbsp_stop_one_bit = 1 @%d\n", 16 + s->end_bit);
}
static void trace_pps(const HevcPps *q)
{
    printf("SECTION PPS\n");
    P("pps_pic_parameter_set_id", q->pps_id); P("pps_seq_parameter_set_id", q->sps_id);
    P("dependent_slice_segments_enabled_flag", q->dependent_slice_segments_enabled); P("output_flag_present_flag", q->output_flag_present);
    P("num_extra_slice_header_bits", q->num_extra_slice_header_bits); P("sign_data_hiding_enabled_flag", q->sign_data_hiding_enabled);
    P("cabac_init_present_flag", q->cabac_init_present);
    P("num_ref_idx_l0_default_active_minus1", q->num_ref_idx_l0_default_active - 1); P("num_ref_idx_l1_default_active_minus1", q->num_ref_idx_l1_default_active - 1);
    P("init_qp_minus26", q->init_qp_minus26); P("constrained_intra_pred_flag", q->constrained_intra_pred); P("transform_skip_enabled_flag", q->transform_skip_enabled);
    P("cu_qp_delta_enabled_flag", q->cu_qp_delta_enabled); if (q->cu_qp_delta_enabled) P("diff_cu_qp_delta_depth", q->diff_cu_qp_delta_depth);
    P("pps_cb_qp_offset", q->cb_qp_offset); P("pps_cr_qp_offset", q->cr_qp_offset);
    P("pps_slice_chroma_qp_offsets_present_flag", q->slice_chroma_qp_offsets_present);
    P("weighted_pred_flag", q->weighted_pred); P("weighted_bipred_flag", q->weighted_bipred); P("transquant_bypass_enabled_flag", q->transquant_bypass_enabled);
    P("tiles_enabled_flag", q->tiles_enabled); P("entropy_coding_sync_enabled_flag", q->entropy_coding_sync_enabled);
    if (q->tiles_enabled) {
        P("num_tile_columns_minus1", q->num_tile_columns - 1); P("num_tile_rows_minus1", q->num_tile_rows - 1); P("uniform_spacing_flag", q->uniform_spacing);
        if (!q->uniform_spacing) {
            for (int i = 0; i < q->num_tile_columns - 1; i++) printf("column_width_minus1[%d] = %d\n", i, q->column_width[i] - 1);
            for (int i = 0; i < q->num_tile_rows - 1; i++) printf("row_height_minus1[%d] = %d\n", i, q->row_height[i] - 1);
        }
        P("loop_filter_across_tiles_enabled_flag", q->loop_filter_across_tiles);
    }
    P("pps_loop_filter_across_slices_enabled_flag", q->loop_filter_across_slices);
    P("deblocking_filter_control_present_flag", q->deblocking_filter_control_present);
    if (q->deblocking_filter_control_present) {
        P("deblocking_filter_override_enabled_flag", q->deblocking_filter_override_enabled); P("pps_deblocking_filter_disabled_flag", q->pps_deblocking_filter_disabled);
        if (!q->pps_deblocking_filter_disabled) { P("pps_beta_offset_div2", q->beta_offset_div2); P("pps_tc_offset_div2", q->tc_offset_div2); }
    }
    P("pps_scaling_list_data_present_flag", q->scaling_list_data_present); P("lists_modification_present_flag", q->lists_modification_present);
    P("log2_parallel_merge_level_minus2", q->log2_parallel_merge_level - 2);
    P("slice_segment_header_extension_present_flag", q->slice_segment_header_extension_present);
    if (q->end_bit) printf("rbsp_stop_one_bit = 1 @%d\n", 16 + q->end_bit);
}
static void trace_slice(const HevcParams *p, const HevcSlice *s)
{
    const HevcPps *q = &p->pps[s->pps_id]; const HevcSps *sps = &p->sps[q->sps_id];
    printf("SECTION SLICE\n");
    P("first_slice_segment_in_pic_flag", s->first_slice_segment_in_pic);
    if (hevc_is_irap(s->nal_type)) P("no_output_of_prior_pics_flag", s->no_output_of_prior_pics);
    P("slice_pic_parameter_set_id", s->pps_id);
    if (!s->first_slice_segment_in_pic) { P("dependent_slice_segment_flag", s->dependent_slice_segment); P("slice_segment_address", s->slice_segment_address); }
    if (!s->dependent_slice_segment) {
        P("slice_type", s->slice_type); if (q->output_flag_present) P("pic_output_flag", s->pic_output_flag);
        if (!hevc_is_idr(s->nal_type)) {
            P("slice_pic_order_cnt_lsb", s->poc_lsb);
            printf("short_term_ref_pic_set_sps_flag = %d @%d\n", s->short_term_ref_pic_set_sps_flag, 16 + s->rps_start);
            if (s->short_term_ref_pic_set_sps_flag && sps->num_short_term_ref_pic_sets > 1) P("short_term_ref_pic_set_idx", s->short_term_ref_pic_set_idx);
            if (sps->long_term_ref_pics_present) { if (sps->num_long_term_ref_pics_sps) P("num_long_term_sps", s->num_long_term_sps); P("num_long_term_pics", s->num_long_term_pics); }
            // the first element after the reference picture set: checks rps_bits
            const char *next = sps->temporal_mvp_enabled ? "slice_temporal_mvp_enabled_flag" : sps->sao_enabled ? "slice_sao_luma_flag"
                             : s->slice_type != HEVC_SLICE_I ? "num_ref_idx_active_override_flag" : "slice_qp_delta";
            printf("%s @%d\n", next, 16 + s->rps_start + s->rps_bits);
            if (sps->temporal_mvp_enabled) P("slice_temporal_mvp_enabled_flag", s->slice_temporal_mvp_enabled);
        }
        if (sps->sao_enabled) { P("slice_sao_luma_flag", s->sao_luma); if (sps->chroma_format_idc) P("slice_sao_chroma_flag", s->sao_chroma); }
        if (s->slice_type != HEVC_SLICE_I) {
            P("num_ref_idx_l0_active_minus1", s->num_ref_idx_active[0] - 1);
            if (s->slice_type == HEVC_SLICE_B) P("num_ref_idx_l1_active_minus1", s->num_ref_idx_active[1] - 1);
            for (int l = 0; l < 2; l++) if (s->ref_pic_list_modification[l]) {
                printf("ref_pic_list_modification_flag_l%d = 1\n", l);
                for (int i = 0; i < s->num_ref_idx_active[l]; i++) printf("list_entry_l%d[%d] = %d\n", l, i, s->list_entry[l][i]);
            }
            if (s->slice_type == HEVC_SLICE_B) P("mvd_l1_zero_flag", s->mvd_l1_zero);
            if (q->cabac_init_present) P("cabac_init_flag", s->cabac_init);
            if (s->slice_temporal_mvp_enabled) { if (s->slice_type == HEVC_SLICE_B) P("collocated_from_l0_flag", s->collocated_from_l0); P("collocated_ref_idx", s->collocated_ref_idx); }
            P("five_minus_max_num_merge_cand", 5 - s->max_num_merge_cand);
        }
        P("slice_qp_delta", s->slice_qp_delta);
        if (q->slice_chroma_qp_offsets_present) { P("slice_cb_qp_offset", s->slice_cb_qp_offset); P("slice_cr_qp_offset", s->slice_cr_qp_offset); }
        if (q->deblocking_filter_override_enabled) P("deblocking_filter_override_flag", s->deblocking_override);
        if (s->deblocking_override) {
            P("slice_deblocking_filter_disabled_flag", s->deblocking_disabled);
            if (!s->deblocking_disabled) { P("slice_beta_offset_div2", s->beta_offset_div2); P("slice_tc_offset_div2", s->tc_offset_div2); }
        }
        P("slice_loop_filter_across_slices_enabled_flag", s->loop_filter_across_slices);
    }
    if (q->tiles_enabled || q->entropy_coding_sync_enabled) { P("num_entry_point_offsets", s->num_entry_point_offsets); if (s->num_entry_point_offsets) P("offset_len_minus1", s->offset_len_minus1); }
    printf("_end = %d\n", 16 + s->header_bits);
}
#undef P

int main(int argc, char **argv)
{
    if (argc < 2) { fprintf(stderr, "usage: hevcparsetest in.hevc\n"); return 2; }
    FILE *fp = fopen(argv[1], "rb"); if (!fp) { perror(argv[1]); return 1; }
    fseek(fp, 0, SEEK_END); const size_t n = (size_t)ftell(fp); fseek(fp, 0, SEEK_SET);
    uint8_t *buf = malloc(n); if (fread(buf, 1, n, fp) != n) return 1;
    static HevcParams p; HevcPocState ps = { 0 };
    const bool tr = getenv("HEVCTRACE") != NULL;
    int pics = 0, slices = 0, errors = 0; bool after_eos = true;
    const uint8_t *nal; size_t len;
    for (size_t pos = 0; hevc_next_annexb(buf, n, &pos, &nal, &len);) {
        const int t = (nal[0] >> 1) & 63, layer = (nal[0] & 1) << 5 | nal[1] >> 3;
        if (layer) continue;
        if (t == HEVC_NAL_SPS) { const int id = hevc_parse_sps(&p, nal, len); if (id < 0) { printf("SPS error\n"); errors++; } else if (tr) trace_sps(&p.sps[id]); }
        if (t == HEVC_NAL_PPS) { const int id = hevc_parse_pps(&p, nal, len); if (id < 0) { printf("PPS error\n"); errors++; } else if (tr) trace_pps(&p.pps[id]); }
        if (t == HEVC_NAL_EOS || t == HEVC_NAL_EOB) after_eos = true;
        if (t <= HEVC_NAL_CRA) {
            HevcSlice s;
            if (hevc_parse_slice_header(&p, nal, len, &s)) { printf("slice %d: header error\n", slices); errors++; continue; }
            slices++;
            if (tr) trace_slice(&p, &s);
            if (s.first_slice_segment_in_pic) {
                const HevcSps *sps = &p.sps[p.pps[s.pps_id].sps_id];
                const bool nro = hevc_is_idr(t) || (t >= HEVC_NAL_BLA_W_LP && t <= 18) || after_eos;
                after_eos = false;
                const int poc = hevc_poc(&ps, sps, &s, nro);
                pics++;
                if (!tr) {
                    printf("pic %d: nal %d type %c poc %d hdr %d bits (%d bytes) rps %d bits st %d bits {", pics - 1, t, "BPI"[s.slice_type % 3], poc,
                           s.header_bits, s.header_bytes, s.rps_bits, s.st_rps_bits);
                    for (int i = 0; i < s.st_rps.num_negative + s.st_rps.num_positive; i++) printf(" %d%s", s.st_rps.delta_poc[i], s.st_rps.used[i] ? "" : "u");
                    printf(" }\n");
                }
            }
        }
    }
    free(buf); fclose(fp);
    if (!tr) printf("RESULT %d pictures, %d slice segments, %d errors\n", pics, slices, errors);
    return errors != 0;
}
