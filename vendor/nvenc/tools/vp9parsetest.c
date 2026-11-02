// vp9parsetest.c (M10, T10.01): parse every frame of a VP9 IVF file with src/vp9parse.c and print the header fields
// (compare with ffmpeg -bsf:v trace_headers). No GPU needed.
//   ./vp9parsetest in.ivf
#include "vp9parse.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv)
{
    if (argc < 2) { fprintf(stderr, "usage: vp9parsetest in.ivf\n"); return 2; }
    FILE *fp = fopen(argv[1], "rb"); if (!fp) { perror(argv[1]); return 1; }
    uint8_t hdr[32]; if (fread(hdr, 1, 32, fp) != 32 || memcmp(hdr, "DKIF", 4)) { fprintf(stderr, "not IVF\n"); return 1; }
    static Vp9State st; vp9_init(&st);
    int chunk = 0, nframes = 0, bad = 0, padbad = 0;
    for (;;) {
        uint8_t fh[12]; if (fread(fh, 1, 12, fp) != 12) break;
        const size_t sz = fh[0] | fh[1] << 8 | fh[2] << 16 | (size_t)fh[3] << 24;
        uint8_t *buf = malloc(sz); if (fread(buf, 1, sz, fp) != sz) break;
        size_t off[8], len[8]; const int nf = vp9_superframe(buf, sz, off, len);
        for (int i = 0; i < nf; i++) {
            Vp9Frame f; const int r = vp9_parse_frame(&st, buf + off[i], len[i], &f);
            if (r) { printf("chunk %d frame %d: PARSE ERROR\n", chunk, i); bad++; continue; }
            if (getenv("VP9TRACE")) {   // "name = value" lines in ffmpeg trace_headers' names (scripts/vp9-trace-compare.py)
                printf("FRAME\nshow_existing_frame = %d\n", f.show_existing_frame);
                if (f.show_existing_frame) { printf("frame_to_show_map_idx = %d\n", f.frame_to_show_map_idx); goto next; }
                printf("frame_type = %d\nshow_frame = %d\nerror_resilient_mode = %d\nintra_only = %d\nreset_frame_context = %d\n"
                       "refresh_frame_flags = %d\nframe_width_minus_1 = %d\nframe_height_minus_1 = %d\nallow_high_precision_mv = %d\n"
                       "refresh_frame_context = %d\nframe_parallel_decoding_mode = %d\nloop_filter_level = %d\nloop_filter_sharpness = %d\n"
                       "base_q_idx = %d\nsegmentation_enabled = %d\nsegmentation_update_map = %d\nsegmentation_temporal_update = %d\n"
                       "segmentation_update_data = %d\ntile_cols_log2 = %d\ntile_rows_log2 = %d\nheader_size_in_bytes = %d\n"
                       "color_space = %d\n",
                       f.frame_type, f.show_frame, f.error_resilient_mode, f.intra_only, f.reset_frame_context, f.refresh_frame_flags,
                       f.width - 1, f.height - 1, f.allow_high_precision_mv, f.refresh_frame_context, f.frame_parallel_decoding_mode,
                       f.lf_level, f.lf_sharpness, f.base_q_idx, f.seg_enabled, f.seg_update_map, f.seg_temporal_update,
                       f.seg_update_data, f.tile_cols_log2, f.tile_rows_log2, f.header_size_in_bytes, f.color_space);
                if (!f.frame_is_intra && !f.error_resilient_mode) printf("frame_context_idx = %d\n", f.frame_context_idx);
                if (!f.frame_is_intra) for (int k = 0; k < 3; k++)
                    printf("ref_frame_idx[%d] = %d\nref_frame_sign_bias[%d] = %d\n", k, f.ref_frame_idx[k], k + 1, f.ref_frame_sign_bias[k + 1]);
                goto next;
            }
            if (f.show_existing_frame) printf("chunk %d.%d: show_existing %d\n", chunk, i, f.frame_to_show_map_idx);
            else {
                printf("chunk %d.%d: type %d show %d intra_only %d err %d %dx%d (ref %d) prof %d bd %d cs %d ss %d%d refresh 0x%02x "
                       "ref %d,%d,%d bias %d%d%d hp %d filt %d ctx %d refctx %d fpd %d reset %d lf %d/%d q %d/%d,%d,%d seg %d/%d/%d/%d "
                       "tiles %d/%d hdr %d+%d tx %d refmode %d pad %d%s\n",
                       chunk, i, f.frame_type, f.show_frame, f.intra_only, f.error_resilient_mode, f.width, f.height, f.size_from_ref,
                       f.profile, f.bit_depth, f.color_space, f.subsampling_x, f.subsampling_y, f.refresh_frame_flags,
                       f.ref_frame_idx[0], f.ref_frame_idx[1], f.ref_frame_idx[2], f.ref_frame_sign_bias[1], f.ref_frame_sign_bias[2],
                       f.ref_frame_sign_bias[3], f.allow_high_precision_mv, f.interp_filter, f.frame_context_idx, f.refresh_frame_context,
                       f.frame_parallel_decoding_mode, f.reset_frame_context, f.lf_level, f.lf_sharpness, f.base_q_idx, f.delta_q_y_dc,
                       f.delta_q_uv_dc, f.delta_q_uv_ac, f.seg_enabled, f.seg_update_map, f.seg_temporal_update, f.seg_update_data,
                       f.tile_cols_log2, f.tile_rows_log2, f.uncomp_bytes, f.header_size_in_bytes, f.tx_mode, f.reference_mode,
                       f.pad_bits, f.pad_nonzero ? " NONZERO-PADDING" : "");
                if (f.pad_nonzero || f.pad_bits < 0) padbad++;
            }
        next:
            vp9_end_frame(&st, &f, NULL);
            nframes++;
        }
        free(buf); chunk++;
    }
    printf("RESULT %d chunks, %d frames, %d parse errors, %d compressed headers with bad padding\n", chunk, nframes, bad, padbad);
    return bad || padbad;
}
