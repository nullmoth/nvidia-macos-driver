// vp9parse.h (M10): VP9 frame headers for the decoder. NVDEC decodes the tiles; the driver parses the uncompressed and
// compressed headers, keeps the four saved probability contexts, applies the compressed header's probability updates and,
// after each frame, adapts the probabilities from the counts the engine writes (backward adaptation). From the VP9
// Bitstream & Decoding Process Specification v0.7 (sections 6.1-6.3, 7.2-7.3, 8.4, 9.2, Annex B); default tables in
// vp9tables.h. The counts come in NVDEC's layout (nvdec_vp9EntropyCounts_t, open-gpu-doc nvdec_drv.h).
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum { VP9_KEY_FRAME = 0, VP9_NON_KEY_FRAME = 1 };
enum { VP9_EIGHTTAP = 0, VP9_EIGHTTAP_SMOOTH = 1, VP9_EIGHTTAP_SHARP = 2, VP9_BILINEAR = 3, VP9_SWITCHABLE = 4 };
enum { VP9_ONLY_4X4 = 0, VP9_ALLOW_8X8 = 1, VP9_ALLOW_16X16 = 2, VP9_ALLOW_32X32 = 3, VP9_TX_MODE_SELECT = 4 };
enum { VP9_SINGLE_REFERENCE = 0, VP9_COMPOUND_REFERENCE = 1, VP9_REFERENCE_MODE_SELECT = 2 };
enum { VP9_INTRA_FRAME = 0, VP9_LAST_FRAME = 1, VP9_GOLDEN_FRAME = 2, VP9_ALTREF_FRAME = 3 };
enum { VP9_CS_RGB = 7 };

// Every adaptive probability table (the spec's names and shapes).
typedef struct {
    uint8_t tx8[2][1], tx16[2][2], tx32[2][3];
    uint8_t coef[4][2][2][6][6][3];
    uint8_t skip[3];
    uint8_t inter_mode[7][3];
    uint8_t interp_filter[4][2];
    uint8_t is_inter[4];
    uint8_t comp_mode[5];
    uint8_t single_ref[5][2];
    uint8_t comp_ref[5];
    uint8_t y_mode[4][9];
    uint8_t uv_mode[10][9];
    uint8_t partition[16][3];
    uint8_t mv_joint[3];
    uint8_t mv_sign[2];
    uint8_t mv_class[2][10];
    uint8_t mv_class0_bit[2];
    uint8_t mv_bits[2][10];
    uint8_t mv_class0_fr[2][2][3];
    uint8_t mv_fr[2][3];
    uint8_t mv_class0_hp[2];
    uint8_t mv_hp[2];
} Vp9Probs;

// One frame's header (uncompressed + compressed).
typedef struct {
    int profile, show_existing_frame, frame_to_show_map_idx;
    int frame_type, show_frame, error_resilient_mode, intra_only, frame_is_intra, reset_frame_context;
    int bit_depth, color_space, color_range, subsampling_x, subsampling_y;
    int refresh_frame_flags, ref_frame_idx[3], ref_frame_sign_bias[4];
    int width, height, render_width, render_height, size_from_ref;
    int allow_high_precision_mv, interp_filter;
    int refresh_frame_context, frame_parallel_decoding_mode, frame_context_idx;
    int lf_level, lf_sharpness;
    int base_q_idx, delta_q_y_dc, delta_q_uv_dc, delta_q_uv_ac, lossless;
    int seg_enabled, seg_update_map, seg_temporal_update, seg_update_data;
    uint8_t seg_tree_probs[7], seg_pred_probs[3];
    int tile_cols_log2, tile_rows_log2;
    int uncomp_bytes, header_size_in_bytes;     // the tile data starts at uncomp_bytes + header_size_in_bytes
    int tx_mode, reference_mode, comp_fixed_ref, comp_var_ref[2];
    int last_frame_type;                        // LastFrameType (the previous frame's frame_type)
    int pad_bits, pad_nonzero;                  // compressed header: bits left after the last symbol, and whether any is set
} Vp9Frame;

// What persists from frame to frame.
typedef struct {
    Vp9Probs ctx[4];                            // the saved frame contexts
    Vp9Probs fc;                                // this frame's probabilities (loaded context + compressed header updates)
    int frame_type;                             // of the last frame parsed (LastFrameType of the next one)
    int bit_depth, color_space, color_range, subsampling_x, subsampling_y, profile;
    int ref_width[8], ref_height[8];            // RefFrameWidth/Height per slot
    int lf_delta_enabled, lf_ref_deltas[4], lf_mode_deltas[2];
    int seg_abs_or_delta_update, seg_feature_enabled[8][4], seg_feature_data[8][4];
    int interp_filter, allow_high_precision_mv;
    bool started;                               // a key frame was seen
} Vp9State;

void vp9_init(Vp9State *s);
// Split a chunk (one container sample) into frames (Annex B superframe index; else the whole chunk is one frame).
// Returns the number of frames, sizes/offsets in off[]/len[] (at most 8).
int  vp9_superframe(const uint8_t *buf, size_t n, size_t off[8], size_t len[8]);
// Parse one frame's uncompressed and compressed headers; updates the state (contexts reset / loaded, s->fc updated).
// Returns 0, or -1 on a malformed header. A show_existing_frame frame has no compressed header (header_size_in_bytes 0).
int  vp9_parse_frame(Vp9State *s, const uint8_t *buf, size_t n, Vp9Frame *f);
// After the frame is decoded: backward adaptation from the engine's counts (an nvdec_vp9EntropyCounts_t; NULL when
// the frame does not adapt), saving the context, the reference slots' sizes (refresh_probs + reference update).
struct nvdec_vp9EntropyCounts_s;
void vp9_end_frame(Vp9State *s, const Vp9Frame *f, const struct nvdec_vp9EntropyCounts_s *counts);
// True when this frame's probabilities are adapted from counts (the engine's counters are needed).
static inline bool vp9_adapts(const Vp9Frame *f) { return !f->error_resilient_mode && !f->frame_parallel_decoding_mode; }
