// av1hdr.h (M12): AV1 OBUs for the encoder. NVENC writes only the tile data; the driver writes the temporal delimiter,
// the sequence header and each frame header (with the base quantizer, loop filter level and CDEF damping the engine
// chose, from its status) and wraps the tiles in a frame OBU. Only the coding tools NVENC's AV1 encoder uses are
// signalled (nvenc_drv.h's "Not supported" list): 64x64 superblocks, no filter intra / intra edge filter / inter-intra /
// masked / joint compound / warped motion / dual filter / reference frame MVs / screen content tools / superres / film
// grain; order hints on. From the AV1 specification (5.3, 5.5, 5.9, 5.11).
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct {
    int width, height;              // frame size (AV1 codes any size; the decoder crops to it)
    int bit_depth;                  // 8 or 10 (profile 0, 4:2:0)
    bool full_range;                // color_range
    int colour[3];                  // color_primaries, transfer_characteristics, matrix_coefficients (H.273); 0 = BT.709 (1)
    int seq_level_idx;              // 0 = from the size at 60 fps
    bool enable_cdef, enable_restoration;
    int order_hint_bits;            // 1..8 (0 = 7)
} Av1SeqParams;

typedef struct {
    int frame_type;                 // 0 key, 1 inter
    bool hidden;                    // inter: show_frame 0 (showable later with show_existing_frame), e.g. a B group's ALTREF
    int order_hint;
    int refresh_frame_flags;        // inter frames (a shown key frame refreshes all)
    int ref_frame_idx[7];           // inter: slot of LAST..ALTREF
    int ref_order_hint[7];          // inter: their order hints (for skip mode, 5.9.22)
    int primary_ref_frame;          // inter: 0..6, or 7 = none (default CDFs)
    bool error_resilient;           // inter frames
    bool disable_cdf_update, disable_frame_end_update_cdf;
    int base_q_idx, dq_y_dc, dq_u_dc, dq_u_ac;
    bool delta_q_present;           // block-level quantizer deltas (delta_q_res 0; no loop filter deltas)
    int lf_level[4], lf_sharpness;
    bool lf_delta_enabled;          // the reference / mode deltas (spec defaults, not updated)
    int cdef_damping;               // 3..6
    int cdef_bits, cdef_y[8], cdef_uv[8];   // strengths as (primary << 2) | secondary (secondary 0..3, 3 meaning 4)
    int lr_type[3], lr_unit_shift;  // loop restoration per plane as coded (0 none, 1 switchable, 2 Wiener, 3 self-guided); unit 64 << shift
    bool tx_mode_select, reference_select, reduced_tx_set;
    int interp_filter;              // 0..3, 4 = switchable
    bool allow_high_precision_mv, is_motion_mode_switchable;
    int tile_cols_log2, tile_rows_log2, context_update_tile_id, tile_size_bytes;   // uniform spacing
} Av1FrameParams;

int    av1_level_for(int width, int height);   // seq_level_idx for this size at 60 fps
size_t av1_write_temporal_delimiter(uint8_t *out);                        // 2 bytes
size_t av1_write_sequence_header(const Av1SeqParams *s, uint8_t *out);    // < 32 bytes
// A frame OBU: the frame header, then a tile group with ntiles tiles (raster order); returns the bytes written
size_t av1_write_frame(const Av1SeqParams *s, const Av1FrameParams *f, const uint8_t *const *tile, const size_t *tile_size,
                       int ntiles, uint8_t *out);
// Metadata OBUs (6.7) for HDR: mastering display colour volume and content light level, from the H.264 / HEVC SEI
// payloads (as VideoToolbox holds them: G B R primaries in 0.00002, luminance in 0.0001 cd/m2); either may be NULL
size_t av1_write_hdr_metadata(const uint8_t *mdcv, const uint8_t *cll, uint8_t *out);
// A frame header OBU with show_existing_frame = 1: show the (inter) frame in slot `slot` (2-3 bytes)
size_t av1_write_show_existing(int slot, uint8_t *out);
