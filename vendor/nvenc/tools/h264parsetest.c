// h264parsetest.c (M8, T8.02): parse an Annex B H.264 stream with src/h264parse.c and print the parameter sets and one
// line per picture (decode order): NAL type, slice type, frame_num, POC, reference flag, MMCO count.
//   ./h264parsetest in.h264 [pts.txt]
// pts.txt: one presentation timestamp per picture in decode order (ffprobe -show_packets on the MP4); then the POC order
// within each IDR period is checked against the timestamp order (they must rank the pictures the same way).
#include "h264parse.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct { int poc, idr_period; double pts; } Pic;

static int cmp_period_then(const void *a, const void *b, int by_pts)
{
    const Pic *x = a, *y = b;
    if (x->idr_period != y->idr_period) return x->idr_period - y->idr_period;
    if (by_pts) return x->pts < y->pts ? -1 : x->pts > y->pts;
    return x->poc - y->poc;
}
static int by_poc(const void *a, const void *b) { return cmp_period_then(a, b, 0); }
static int by_pts(const void *a, const void *b) { return cmp_period_then(a, b, 1); }

int main(int argc, char **argv)
{
    if (argc < 2) { fprintf(stderr, "usage: h264parsetest in.h264 [pts.txt]\n"); return 2; }
    FILE *f = fopen(argv[1], "rb"); if (!f) { perror(argv[1]); return 1; }
    fseek(f, 0, SEEK_END); long n = ftell(f); fseek(f, 0, SEEK_SET);
    uint8_t *b = malloc((size_t)n); if (fread(b, 1, (size_t)n, f) != (size_t)n) return 1; fclose(f);
    static H264Params p; H264PocState st = { 0 };
    static Pic pics[100000]; int np = 0, period = -1, errors = 0, nslices = 0;
    size_t pos = 0; const uint8_t *nal; size_t len;
    while (h264_next_annexb(b, (size_t)n, &pos, &nal, &len)) {
        int t = nal[0] & 31;
        if (t == H264_NAL_SPS) {
            if (h264_parse_sps(&p, nal, len)) { printf("SPS: PARSE ERROR\n"); errors++; continue; }
            for (int i = 0; i < 32; i++) if (p.sps[i].valid) {
                const H264Sps *s = &p.sps[i];
                printf("SPS %d: profile %d level %d chroma %d depth %d/%d %dx%d mbs %dx%d frame_mbs_only %d mbaff %d poc_type %d "
                       "log2_max_frame_num %d max_ref %d reorder %d dpb %d scaling %d full_range %d\n", i, s->profile_idc, s->level_idc,
                       s->chroma_format_idc, s->bit_depth_luma_minus8 + 8, s->bit_depth_chroma_minus8 + 8, s->width, s->height,
                       s->pic_width_in_mbs_minus1 + 1, s->pic_height_in_map_units_minus1 + 1, s->frame_mbs_only_flag,
                       s->mb_adaptive_frame_field_flag, s->pic_order_cnt_type, s->log2_max_frame_num_minus4 + 4, s->max_num_ref_frames,
                       s->max_num_reorder_frames, s->max_dec_frame_buffering, s->seq_scaling_matrix_present_flag, s->video_full_range_flag);
                break;
            }
        } else if (t == H264_NAL_PPS) {
            if (h264_parse_pps(&p, nal, len)) { printf("PPS: PARSE ERROR\n"); errors++; continue; }
            for (int i = 0; i < 256; i++) if (p.pps[i].valid) {
                const H264Pps *q = &p.pps[i];
                printf("PPS %d: cabac %d refs %d/%d weighted %d/%d qp %d cqp %d/%d deblock_ctl %d cip %d t8x8 %d scaling %d "
                       "4x4[0][0..3] %d %d %d %d\n", i, q->entropy_coding_mode_flag, q->num_ref_idx_l0_default_active_minus1 + 1,
                       q->num_ref_idx_l1_default_active_minus1 + 1, q->weighted_pred_flag, q->weighted_bipred_idc, q->pic_init_qp_minus26 + 26,
                       q->chroma_qp_index_offset, q->second_chroma_qp_index_offset, q->deblocking_filter_control_present_flag,
                       q->constrained_intra_pred_flag, q->transform_8x8_mode_flag, q->pic_scaling_matrix_present_flag,
                       q->scaling4x4[0][0], q->scaling4x4[0][1], q->scaling4x4[0][2], q->scaling4x4[0][3]);
                break;
            }
        } else if (t == H264_NAL_SLICE || t == H264_NAL_IDR) {
            H264Slice s;
            if (h264_parse_slice_header(&p, nal, len, &s)) { printf("slice: PARSE ERROR (nal %d)\n", t); errors++; continue; }
            nslices++;
            if (s.first_mb_in_slice != 0) continue;                    // one line per picture (its first slice)
            const H264Sps *sps = &p.sps[p.pps[s.pps_id].sps_id];
            int top, bot; h264_poc(&st, sps, &s, &top, &bot);
            if (s.idr) period++;
            if (np < 100000) pics[np] = (Pic){ s.field_pic_flag ? (s.bottom_field_flag ? bot : top) : (top < bot ? top : bot), period, 0 };
            if (np < 40 || getenv("ALL"))
                printf("pic %4d: nal %d ref %d %c frame_num %3d %s poc %5d/%5d mmco %d%s\n", np, s.nal_unit_type, s.nal_ref_idc,
                       "PBIps"[s.slice_type], s.frame_num, s.field_pic_flag ? (s.bottom_field_flag ? "bot" : "top") : "frm",
                       top, bot, s.n_mmco, s.has_mmco5 ? " (mmco5)" : "");
            np++;
        }
    }
    printf("%d pictures, %d slices, %d parse errors\n", np, nslices, errors);
    if (argc > 2) {
        FILE *pf = fopen(argv[2], "r"); int k = 0; double v;
        while (pf && k < np && fscanf(pf, "%lf", &v) == 1) pics[k++].pts = v;
        if (pf) fclose(pf);
        if (k != np) { printf("PTS: %d timestamps for %d pictures - not compared\n", k, np); return errors ? 1 : 0; }
        static Pic a[100000], c[100000]; memcpy(a, pics, sizeof(Pic) * (size_t)np); memcpy(c, pics, sizeof(Pic) * (size_t)np);
        qsort(a, (size_t)np, sizeof(Pic), by_poc); qsort(c, (size_t)np, sizeof(Pic), by_pts);
        int bad = 0; for (int i = 0; i < np; i++) if (a[i].pts != c[i].pts) bad++;
        printf("RESULT POC order %s timestamp order (%d of %d positions differ)\n", bad ? "DIFFERS FROM" : "matches", bad, np);
        return (errors || bad) ? 1 : 0;
    }
    return errors ? 1 : 0;
}
