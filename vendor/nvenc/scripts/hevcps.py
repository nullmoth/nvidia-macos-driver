#!/usr/bin/env python3
# hevcps.py: decode the VPS / SPS / PPS and the first slice segment header of each picture in an Annex B HEVC stream
# (ITU-T H.265 7.3), to compare streams (nullmoth's library vs ours). Single layer, 8-bit 4:2:0 as written by NVENC.
#   scripts/hevcps.py stream.h265 [--slices]
import sys

class Bits:
    def __init__(self, b):
        # RBSP: drop emulation prevention bytes (00 00 03)
        out, z = bytearray(), 0
        for x in b:
            if z >= 2 and x == 3: z = 0; continue
            out.append(x); z = z + 1 if x == 0 else 0
        self.b, self.p = bytes(out), 0
    def u(self, n):
        v = 0
        for _ in range(n):
            v = v << 1 | (self.b[self.p >> 3] >> (7 - (self.p & 7)) & 1); self.p += 1
        return v
    def ue(self):
        z = 0
        while self.u(1) == 0: z += 1
        return (1 << z) - 1 + self.u(z)
    def se(self):
        k = self.ue(); return (k + 1) // 2 if k & 1 else -(k // 2)

def nals(data):
    i, starts = 0, []
    while True:
        j = data.find(b'\x00\x00\x01', i)
        if j < 0: break
        starts.append(j + 3); i = j + 3
    for k, s in enumerate(starts):
        e = starts[k + 1] - 3 if k + 1 < len(starts) else len(data)
        while e > s and data[e - 1] == 0: e -= 1
        yield data[s:e]

def ptl(r, out, max_sub_layers_minus1):
    out['general_profile_space'] = r.u(2); out['general_tier_flag'] = r.u(1); out['general_profile_idc'] = r.u(5)
    out['general_profile_compatibility_flags'] = '%08x' % r.u(32)
    out['progressive_source_flag'] = r.u(1); out['interlaced_source_flag'] = r.u(1)
    out['non_packed_constraint_flag'] = r.u(1); out['frame_only_constraint_flag'] = r.u(1)
    r.u(32); r.u(11)   # 43 reserved/constraint bits
    out['general_inbld_flag'] = r.u(1)
    out['general_level_idc'] = r.u(8)
    assert max_sub_layers_minus1 == 0, "sub-layers not handled"

def vps(r):
    o = {}
    o['vps_video_parameter_set_id'] = r.u(4); o['vps_base_layer_internal_flag'] = r.u(1); o['vps_base_layer_available_flag'] = r.u(1)
    o['vps_max_layers_minus1'] = r.u(6); o['vps_max_sub_layers_minus1'] = n = r.u(3); o['vps_temporal_id_nesting_flag'] = r.u(1)
    o['vps_reserved_0xffff_16bits'] = '%04x' % r.u(16)
    ptl(r, o, n)
    o['vps_sub_layer_ordering_info_present_flag'] = r.u(1)
    o['vps_max_dec_pic_buffering_minus1'] = r.ue(); o['vps_max_num_reorder_pics'] = r.ue(); o['vps_max_latency_increase_plus1'] = r.ue()
    o['vps_max_layer_id'] = r.u(6); o['vps_num_layer_sets_minus1'] = r.ue()
    o['vps_timing_info_present_flag'] = t = r.u(1)
    if t:
        o['vps_num_units_in_tick'] = r.u(32); o['vps_time_scale'] = r.u(32)
        o['vps_poc_proportional_to_timing_flag'] = p = r.u(1)
        if p: o['vps_num_ticks_poc_diff_one_minus1'] = r.ue()
        o['vps_num_hrd_parameters'] = r.ue()
    o['vps_extension_flag'] = r.u(1)
    return o

def st_rps(r, idx, num):
    o = {}
    if idx: o['inter_ref_pic_set_prediction_flag'] = r.u(1)
    o['num_negative_pics'] = nn = r.ue(); o['num_positive_pics'] = np = r.ue()
    o['s0'] = [(r.ue() + 1, r.u(1)) for _ in range(nn)]
    o['s1'] = [(r.ue() + 1, r.u(1)) for _ in range(np)]
    return o

def sps(r):
    o = {}
    o['sps_video_parameter_set_id'] = r.u(4); o['sps_max_sub_layers_minus1'] = n = r.u(3); o['sps_temporal_id_nesting_flag'] = r.u(1)
    ptl(r, o, n)
    o['sps_seq_parameter_set_id'] = r.ue(); o['chroma_format_idc'] = c = r.ue()
    if c == 3: o['separate_colour_plane_flag'] = r.u(1)
    o['pic_width_in_luma_samples'] = r.ue(); o['pic_height_in_luma_samples'] = r.ue()
    o['conformance_window_flag'] = w = r.u(1)
    if w: o['conf_win'] = [r.ue() for _ in range(4)]
    o['bit_depth_luma_minus8'] = r.ue(); o['bit_depth_chroma_minus8'] = r.ue()
    o['log2_max_pic_order_cnt_lsb_minus4'] = r.ue()
    o['sps_sub_layer_ordering_info_present_flag'] = r.u(1)
    o['sps_max_dec_pic_buffering_minus1'] = r.ue(); o['sps_max_num_reorder_pics'] = r.ue(); o['sps_max_latency_increase_plus1'] = r.ue()
    o['log2_min_luma_coding_block_size_minus3'] = r.ue(); o['log2_diff_max_min_luma_coding_block_size'] = r.ue()
    o['log2_min_luma_transform_block_size_minus2'] = r.ue(); o['log2_diff_max_min_luma_transform_block_size'] = r.ue()
    o['max_transform_hierarchy_depth_inter'] = r.ue(); o['max_transform_hierarchy_depth_intra'] = r.ue()
    o['scaling_list_enabled_flag'] = sl = r.u(1)
    assert not sl, "scaling lists not handled"
    o['amp_enabled_flag'] = r.u(1); o['sample_adaptive_offset_enabled_flag'] = r.u(1); o['pcm_enabled_flag'] = pcm = r.u(1)
    assert not pcm, "pcm not handled"
    o['num_short_term_ref_pic_sets'] = ns = r.ue()
    o['st_ref_pic_sets'] = [st_rps(r, i, ns) for i in range(ns)]
    o['long_term_ref_pics_present_flag'] = lt = r.u(1)
    assert not lt, "long-term refs not handled"
    o['sps_temporal_mvp_enabled_flag'] = r.u(1); o['strong_intra_smoothing_enabled_flag'] = r.u(1)
    o['vui_parameters_present_flag'] = v = r.u(1)
    if v:
        vu = {}
        vu['aspect_ratio_info_present_flag'] = a = r.u(1)
        if a:
            vu['aspect_ratio_idc'] = ai = r.u(8)
            if ai == 255: vu['sar'] = (r.u(16), r.u(16))
        vu['overscan_info_present_flag'] = ov = r.u(1)
        if ov: vu['overscan_appropriate_flag'] = r.u(1)
        vu['video_signal_type_present_flag'] = vs = r.u(1)
        if vs:
            vu['video_format'] = r.u(3); vu['video_full_range_flag'] = r.u(1); vu['colour_description_present_flag'] = cd = r.u(1)
            if cd: vu['colour_primaries'] = r.u(8); vu['transfer_characteristics'] = r.u(8); vu['matrix_coeffs'] = r.u(8)
        vu['chroma_loc_info_present_flag'] = cl = r.u(1)
        if cl: vu['chroma_loc'] = (r.ue(), r.ue())
        vu['neutral_chroma_indication_flag'] = r.u(1); vu['field_seq_flag'] = r.u(1); vu['frame_field_info_present_flag'] = r.u(1)
        vu['default_display_window_flag'] = dw = r.u(1)
        if dw: vu['def_disp_win'] = [r.ue() for _ in range(4)]
        vu['vui_timing_info_present_flag'] = ti = r.u(1)
        if ti:
            vu['vui_num_units_in_tick'] = r.u(32); vu['vui_time_scale'] = r.u(32); vu['vui_poc_proportional_to_timing_flag'] = pp = r.u(1)
            if pp: vu['vui_num_ticks_poc_diff_one_minus1'] = r.ue()
            vu['vui_hrd_parameters_present_flag'] = h = r.u(1)
            assert not h, "HRD not handled"
        vu['bitstream_restriction_flag'] = br = r.u(1)
        if br:
            vu['tiles_fixed_structure_flag'] = r.u(1); vu['motion_vectors_over_pic_boundaries_flag'] = r.u(1)
            vu['restricted_ref_pic_lists_flag'] = r.u(1); vu['min_spatial_segmentation_idc'] = r.ue()
            vu['max_bytes_per_pic_denom'] = r.ue(); vu['max_bits_per_min_cu_denom'] = r.ue()
            vu['log2_max_mv_length_horizontal'] = r.ue(); vu['log2_max_mv_length_vertical'] = r.ue()
        o['vui'] = vu
    o['sps_extension_present_flag'] = r.u(1)
    return o

def pps(r):
    o = {}
    o['pps_pic_parameter_set_id'] = r.ue(); o['pps_seq_parameter_set_id'] = r.ue()
    o['dependent_slice_segments_enabled_flag'] = r.u(1); o['output_flag_present_flag'] = r.u(1); o['num_extra_slice_header_bits'] = r.u(3)
    o['sign_data_hiding_enabled_flag'] = r.u(1); o['cabac_init_present_flag'] = r.u(1)
    o['num_ref_idx_l0_default_active_minus1'] = r.ue(); o['num_ref_idx_l1_default_active_minus1'] = r.ue()
    o['init_qp_minus26'] = r.se(); o['constrained_intra_pred_flag'] = r.u(1); o['transform_skip_enabled_flag'] = r.u(1)
    o['cu_qp_delta_enabled_flag'] = cq = r.u(1)
    if cq: o['diff_cu_qp_delta_depth'] = r.ue()
    o['pps_cb_qp_offset'] = r.se(); o['pps_cr_qp_offset'] = r.se(); o['pps_slice_chroma_qp_offsets_present_flag'] = r.u(1)
    o['weighted_pred_flag'] = r.u(1); o['weighted_bipred_flag'] = r.u(1); o['transquant_bypass_enabled_flag'] = r.u(1)
    o['tiles_enabled_flag'] = t = r.u(1); o['entropy_coding_sync_enabled_flag'] = r.u(1)
    assert not t, "tiles not handled"
    o['pps_loop_filter_across_slices_enabled_flag'] = r.u(1); o['deblocking_filter_control_present_flag'] = d = r.u(1)
    if d:
        o['deblocking_filter_override_enabled_flag'] = r.u(1); o['pps_deblocking_filter_disabled_flag'] = dd = r.u(1)
        if not dd: o['pps_beta_offset_div2'] = r.se(); o['pps_tc_offset_div2'] = r.se()
    o['pps_scaling_list_data_present_flag'] = r.u(1); o['lists_modification_present_flag'] = r.u(1)
    o['log2_parallel_merge_level_minus2'] = r.ue(); o['slice_segment_header_extension_present_flag'] = r.u(1)
    o['pps_extension_present_flag'] = r.u(1)
    return o

def slice_header(r, t, S, P):
    o = {}
    o['first_slice_segment_in_pic_flag'] = first = r.u(1)
    if 16 <= t <= 23: o['no_output_of_prior_pics_flag'] = r.u(1)
    o['slice_pic_parameter_set_id'] = r.ue()
    if not first: return o   # (dependent slices / addresses not decoded)
    for i in range(P['num_extra_slice_header_bits']): r.u(1)
    o['slice_type'] = st = r.ue()   # 0 B, 1 P, 2 I
    if P['output_flag_present_flag']: o['pic_output_flag'] = r.u(1)
    if t not in (19, 20):
        o['slice_pic_order_cnt_lsb'] = r.u(S['log2_max_pic_order_cnt_lsb_minus4'] + 4)
        o['short_term_ref_pic_set_sps_flag'] = f = r.u(1)
        if not f: o['st_ref_pic_set'] = st_rps(r, S['num_short_term_ref_pic_sets'], S['num_short_term_ref_pic_sets'])
        elif S['num_short_term_ref_pic_sets'] > 1:
            import math; o['short_term_ref_pic_set_idx'] = r.u(math.ceil(math.log2(S['num_short_term_ref_pic_sets'])))
        if S['sps_temporal_mvp_enabled_flag']: o['slice_temporal_mvp_enabled_flag'] = r.u(1)
    if S['sample_adaptive_offset_enabled_flag']: o['slice_sao_luma_flag'] = r.u(1); o['slice_sao_chroma_flag'] = r.u(1)
    if st != 2:
        o['num_ref_idx_active_override_flag'] = ov = r.u(1)
        if ov:
            o['num_ref_idx_l0_active_minus1'] = r.ue()
            if st == 0: o['num_ref_idx_l1_active_minus1'] = r.ue()
        if st == 0: o['mvd_l1_zero_flag'] = r.u(1)
        if P['cabac_init_present_flag']: o['cabac_init_flag'] = r.u(1)
        if o.get('slice_temporal_mvp_enabled_flag'):
            if st == 0: o['collocated_from_l0_flag'] = r.u(1)
        o['five_minus_max_num_merge_cand'] = r.ue()
    o['slice_qp_delta'] = r.se()
    if P['pps_slice_chroma_qp_offsets_present_flag']: o['slice_cb_qp_offset'] = r.se(); o['slice_cr_qp_offset'] = r.se()
    if P.get('deblocking_filter_override_enabled_flag'): o['deblocking_filter_override_flag'] = r.u(1)
    o['slice_loop_filter_across_slices_enabled_flag'] = r.u(1) if P['pps_loop_filter_across_slices_enabled_flag'] else None
    return o

def main():
    data = open(sys.argv[1], 'rb').read(); show_slices = '--slices' in sys.argv
    S = P = None; n = 0
    for nal in nals(data):
        t = nal[0] >> 1 & 0x3f; r = Bits(nal[2:])
        if t == 32: print('VPS', vps(r))
        elif t == 33: S = sps(r); print('SPS', S)
        elif t == 34: P = pps(r); print('PPS', P)
        elif t < 32:
            n += 1
            if show_slices or n <= 3: print('slice nal_unit_type %d, %d bytes:' % (t, len(nal)), slice_header(r, t, S, P))
        else: print('NAL type %d, %d bytes' % (t, len(nal)))

main()
