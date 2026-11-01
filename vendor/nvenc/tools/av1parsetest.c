// av1parsetest.c (M9, T9.01): parse every OBU of an AV1 IVF file with src/av1parse.c. With AV1TRACE=1 prints the frame
// header fields as "name = value" in ffmpeg trace_headers' names (scripts/vp9-trace-compare.py av1 ...); else a summary.
//   ./av1parsetest in.ivf
#include "av1parse.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void trace(const Av1State *st, const Av1Frame *h)
{
    const Av1Seq *s = &st->seq;
    printf("FRAME\nshow_existing_frame = %d\n", h->show_existing_frame);
    if (h->show_existing_frame) { printf("frame_to_show_map_idx = %d\n", h->frame_to_show_map_idx); return; }
#define P(n, v) printf("%s = %d\n", n, (int)(v))
    P("frame_type", h->frame_type); P("show_frame", h->show_frame); P("showable_frame", h->showable_frame);
    P("error_resilient_mode", h->error_resilient_mode); P("disable_cdf_update", h->disable_cdf_update);
    P("allow_screen_content_tools", h->allow_screen_content_tools); if (!h->frame_is_intra) P("force_integer_mv", h->force_integer_mv);
    P("frame_size_override_flag", h->frame_size_override_flag); P("order_hint", h->order_hint);
    P("primary_ref_frame", h->primary_ref_frame); P("refresh_frame_flags", h->refresh_frame_flags);
    P("frame_width_minus_1", h->upscaled_width - 1); P("frame_height_minus_1", h->frame_height - 1); P("use_superres", h->use_superres);
    if (h->use_superres) P("coded_denom", h->superres_denom - 9);
    P("allow_intrabc", h->allow_intrabc); P("frame_refs_short_signaling", h->frame_refs_short_signaling);
    if (!h->frame_is_intra) for (int i = 0; i < 7; i++) printf("ref_frame_idx[%d] = %d\n", i, h->ref_frame_idx[i]);
    P("allow_high_precision_mv", h->allow_high_precision_mv);
    P("is_filter_switchable", h->interpolation_filter == AV1_SWITCHABLE);
    if (h->interpolation_filter != AV1_SWITCHABLE) P("interpolation_filter", h->interpolation_filter);
    P("is_motion_mode_switchable", h->is_motion_mode_switchable); P("use_ref_frame_mvs", h->use_ref_frame_mvs);
    P("disable_frame_end_update_cdf", h->disable_frame_end_update_cdf); P("uniform_tile_spacing_flag", h->uniform_tile_spacing);
    P("tile_cols_log2", h->tile_cols_log2); P("tile_rows_log2", h->tile_rows_log2);
    if (h->tile_cols_log2 || h->tile_rows_log2) { P("context_update_tile_id", h->context_update_tile_id); P("tile_size_bytes_minus_1", h->tile_size_bytes - 1); }
    P("base_q_idx", h->base_q_idx); P("diff_uv_delta", h->diff_uv_delta); P("using_qmatrix", h->using_qmatrix);
    if (h->using_qmatrix) { P("qm_y", h->qm_y); P("qm_u", h->qm_u); P("qm_v", h->qm_v); }
    P("segmentation_enabled", h->seg_enabled); P("segmentation_update_map", h->seg_update_map);
    P("segmentation_temporal_update", h->seg_temporal_update); P("segmentation_update_data", h->seg_update_data);
    P("delta_q_present", h->delta_q_present); P("delta_q_res", h->delta_q_res); P("delta_lf_present", h->delta_lf_present);
    P("delta_lf_res", h->delta_lf_res); P("delta_lf_multi", h->delta_lf_multi);
    for (int i = 0; i < 4; i++) printf("loop_filter_level[%d] = %d\n", i, h->lf_level[i]);
    P("loop_filter_sharpness", h->lf_sharpness); P("loop_filter_delta_enabled", h->lf_delta_enabled); P("loop_filter_delta_update", h->lf_delta_update);
    P("cdef_damping_minus_3", h->cdef_damping - 3); P("cdef_bits", h->cdef_bits);
    for (int i = 0; i < (1 << h->cdef_bits); i++) {
        printf("cdef_y_pri_strength[%d] = %d\ncdef_y_sec_strength[%d] = %d\n", i, h->cdef_y_pri[i], i, h->cdef_y_sec[i] == 4 ? 3 : h->cdef_y_sec[i]);
        if (!s->mono_chrome) printf("cdef_uv_pri_strength[%d] = %d\ncdef_uv_sec_strength[%d] = %d\n", i, h->cdef_uv_pri[i], i, h->cdef_uv_sec[i] == 4 ? 3 : h->cdef_uv_sec[i]);
    }
    static const int inv[4] = { 0, 2, 3, 1 };   // FrameRestorationType -> lr_type
    if (s->enable_restoration && !h->all_lossless && !h->allow_intrabc) for (int i = 0; i < (s->mono_chrome ? 1 : 3); i++) printf("lr_type[%d] = %d\n", i, inv[h->lr_type[i]]);
    P("tx_mode_select", h->tx_mode == AV1_TX_MODE_SELECT); P("reference_select", h->reference_select);
    P("skip_mode_present", h->skip_mode_present); P("allow_warped_motion", h->allow_warped_motion); P("reduced_tx_set", h->reduced_tx_set);
    if (!h->frame_is_intra) for (int rf = 1; rf <= 7; rf++) {
        printf("is_global[%d] = %d\n", rf, h->gm_type[rf] != AV1_IDENTITY);
    }
    P("apply_grain", h->fg.apply_grain);
    if (h->fg.apply_grain) { P("grain_seed", h->fg.grain_seed); P("num_y_points", h->fg.num_y_points); }
#undef P
}

int main(int argc, char **argv)
{
    if (argc < 2) { fprintf(stderr, "usage: av1parsetest in.ivf\n"); return 2; }
    FILE *fp = fopen(argv[1], "rb"); if (!fp) { perror(argv[1]); return 1; }
    uint8_t hdr[32]; if (fread(hdr, 1, 32, fp) != 32 || memcmp(hdr, "DKIF", 4)) { fprintf(stderr, "not IVF\n"); return 1; }
    static Av1State st; static Av1Frame f; static Av1Tile tiles[AV1_MAX_TILES];
    const bool tr = getenv("AV1TRACE") != NULL;
    int tus = 0, frames = 0, errors = 0, tilesSeen = 0; bool haveHdr = false;
    for (;;) {
        uint8_t fh[12]; if (fread(fh, 1, 12, fp) != 12) break;
        const size_t sz = fh[0] | fh[1] << 8 | fh[2] << 16 | (size_t)fh[3] << 24;
        uint8_t *buf = malloc(sz); if (fread(buf, 1, sz, fp) != sz) break;
        for (size_t pos = 0; pos < sz;) {
            Av1Obu o; if (av1_obu(buf + pos, sz - pos, &o)) { printf("TU %d: bad OBU at %zu\n", tus, pos); errors++; break; }
            const uint8_t *pl = buf + pos + o.header_len;
            st.temporal_id = o.temporal_id; st.spatial_id = o.spatial_id;
            if (o.type == AV1_OBU_SEQUENCE_HEADER) { if (av1_parse_sequence_header(&st.seq, pl, o.payload_len)) { printf("TU %d: sequence header error\n", tus); errors++; } }
            else if (o.type == AV1_OBU_FRAME_HEADER || o.type == AV1_OBU_FRAME) {
                size_t hb;
                if (av1_parse_frame_header(&st, pl, o.payload_len, &f, &hb)) { printf("TU %d: frame header error\n", tus); errors++; }
                else {
                    haveHdr = true; frames++;
                    if (tr) trace(&st, &f);
                    else printf("TU %d: %s type %d show %d %dx%d (up %d) q %d tiles %dx%d refresh 0x%02x primary %d hdr %zu bytes\n", tus,
                                f.show_existing_frame ? "show_existing" : o.type == AV1_OBU_FRAME ? "frame" : "frame_header", f.frame_type, f.show_frame,
                                f.frame_width, f.frame_height, f.upscaled_width, f.base_q_idx, f.tile_cols, f.tile_rows, f.refresh_frame_flags, f.primary_ref_frame, hb);
                    if (f.show_existing_frame) { av1_end_frame(&st, &f); haveHdr = false; }
                    else if (o.type == AV1_OBU_FRAME) {
                        int tgs; bool last;
                        const int nt = av1_parse_tile_group(&f, pl + hb, o.payload_len - hb, tiles, AV1_MAX_TILES, &tgs, &last);
                        if (nt < 0) { printf("TU %d: tile group error\n", tus); errors++; }
                        else { tilesSeen += nt; if (last) { av1_end_frame(&st, &f); haveHdr = false; } }
                    }
                }
            } else if (o.type == AV1_OBU_TILE_GROUP && haveHdr) {
                int tgs; bool last;
                const int nt = av1_parse_tile_group(&f, pl, o.payload_len, tiles, AV1_MAX_TILES, &tgs, &last);
                if (nt < 0) { printf("TU %d: tile group error\n", tus); errors++; }
                else { tilesSeen += nt; if (last) { av1_end_frame(&st, &f); haveHdr = false; } }
            }
            pos += o.header_len + o.payload_len;
        }
        free(buf); tus++;
    }
    if (!tr) printf("RESULT %d temporal units, %d frame headers, %d tiles, %d errors\n", tus, frames, tilesSeen, errors);
    return errors != 0;
}
