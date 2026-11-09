// enc.h: an H.264, HEVC or AV1 encode session on one NVENC engine: buffers, channel, picture setup per frame, IDR and P pictures,
// rate control, slices. enc_encode() is synchronous; enc_submit()/enc_wait() keep up to `depth` frames in flight. Structures: NVIDIA open-gpu-doc
// nvenc_drv.h / clc9b7.h; what the docs leave out is in docs/design.md ("M2 notes", "M3 notes").
#pragma once
#include "channel.h"
#include <stdint.h>

#define ENC_MAX_REFS 4             // AV1; H.264 and HEVC take up to 3
#define ENC_MAX_SLICES 16
#define ENC_MAX_DEPTH 3

enum { ENC_H264 = 0, ENC_HEVC = 1, ENC_AV1 = 2 };

typedef struct {
    int codec;                      // ENC_H264, ENC_HEVC or ENC_AV1 (qp_i / qp_p are then AV1 quantizer indices, 0..255)
    int width, height;              // display size; coded size is whole macroblocks (H.264) / 16-pixel CBs (HEVC)
    int qp_i, qp_p;                 // constant QP for I/IDR and P pictures
    int num_refs;                   // L0 reference frames, 1..3 (AV1: 1..4, LAST / LAST2 / LAST3 / GOLDEN)
    int dpb_extra;                  // extra pictures announced in the HEVC VPS/SPS decoded picture buffer size, 0..2
                                    // (refs + 1 + 2 <= 6 = the smallest MaxDpbSize of any level)
    int engine;                     // 0 = NVENC0, 1 = NVENC1
    int idr_period;                 // 0 = only the first picture (and enc_encode(force_idr)); with enc_submit_ex (display
                                    // indices) the caller forces the IDRs; the period still goes to the rate control
    int rc_mode;                    // SET_CONTROL_PARAMS RCMODE: 0 const QP, 1 VBR, 2 CBR, 17/18 MB-row VBR/CBR
    int bitrate, max_bitrate;       // bits/s (rate control modes)
    int vbv_bits;                   // CPB size in bits (0 = one second)
    int fps;                        // frame rate for rate control (default 60)
    int slices;                     // slices per picture, whole MB rows (1..ENC_MAX_SLICES)
    bool spin;                      // wait for the engine by spinning only (default: spin 50 us, then sleep on the NVENC event)
    int depth;                      // frames in flight with enc_submit (1..ENC_MAX_DEPTH, default 1)
    int h264_profile;               // H.264: 77 Main (default) or 100 High (adds the 8x8 transform and 8x8 intra modes)
    bool cabac;                     // H.264: CABAC entropy coding (else CAVLC; always CAVLC in 4:4:4)
    int bit_depth;                  // HEVC: 8 (Main, default) or 10 (Main 10: 16-bit input samples, enc_load_p010)
    bool chroma444;                 // HEVC Main 4:4:4 / H.264 High 4:4:4 Predictive (enc_load_yuv444 / enc_load_nv24)
    bool lossless;                  // H.264 (High 4:4:4 Predictive, CAVLC) / HEVC: every macroblock / CU at QP 0, bypassed
    bool quality;                   // H.264: NVIDIA's -preset p4 ME / MD / quant controls for I and P pictures (about +0.6 dB at
                                    // equal rate on camera content, ~2.7 ms instead of ~1.5 ms per 1080p frame); AV1: the default
                                    // preset's MD / ME controls, 4 CDEF strengths and luma loop restoration (T12.09); default: fast
    bool no_cu8;                    // HEVC quality mode without 8x8 coding units (NVIDIA's p7 controls, 16x16 minimum): at 4K
                                    // 10 ms per frame instead of 22, +0.6 VMAF over the fast controls instead of +0.7 (T7.10)
    int bframes;                    // B pictures between anchors (H.264: POC type 0, VUI reordering; AV1: hidden ALTREF anchors);
                                    // the caller orders the pictures and marks the B ones (enc_submit_ex). num_refs >= 2
    int aq;                         // spatial adaptive quantization strength 1..15 as NVIDIA's -aq-strength (8 its default); 0 = off.
                                    // HEVC; from the luma of NV12 / P010 input (enc_load_nv12 / _p010), a QP offset per 32x32 CTB
    bool two_pass;                  // H.264 / HEVC with rate control: two passes per picture at full resolution, as NVIDIA's
                                    // -multipass fullres: a quick first pass writes picture and row statistics into the RC data
                                    // (RCSTAT_WRITE), the second reads them for its QP choices and takes the first pass's motion
                                    // vectors as hints (T7.09; HEVC: I / P pictures only). Over 900 frames -0.25..+0.65 VMAF at
                                    // equal rate for 2x (H.264) / +7 % (HEVC) engine time: off by default
    bool la_pass;                   // H.264: a lookahead analysis session (la.c): the driver's ME / MD / quant controls for its
                                    // QP 40 quarter-size pictures
    bool full_range;                // input is full-range (VUI video_full_range_flag); BGRA input is converted to video range
    int colour[3];                  // colour_primaries, transfer_characteristics, matrix_coefficients (ITU-T H.273) for the VUI /
                                    // AV1 color_config; 0 = BT.709 (1). E.g. BT.2020 PQ: 9, 16, 9; HLG: 9, 18, 9; BT.601 NTSC: 6, 6, 6
    bool has_mdcv, has_cll;         // HDR metadata on every key frame (H.264 / HEVC SEI, AV1 metadata OBUs)
    uint8_t mdcv[24], cll[4];       // as the SEI payloads (VideoToolbox's MasteringDisplayColorVolume / ContentLightLevelInfo)
    bool verbose;
} EncConfig;

typedef struct {
    const uint8_t *data;            // Annex B slice NAL(s) in the session's bitstream buffer (valid until that slot is reused);
                                    // AV1: a temporal unit (temporal delimiter, sequence header on key frames, frame OBU)
    size_t size;
    bool idr;
    unsigned frame_num, poc;
    unsigned bits, ucode_error, avg_qp, intra_mbs, inter_mbs;
    unsigned intra_cost, inter_cost;    // sums of each block's best intra / inter SATD (status total_intra_cost / _inter_cost)
    double ms;                      // doorbell to completion seen
    bool hidden;                    // AV1: a hidden anchor (size 0: it travels with the next frame; show it later with
                                    // enc_av1_show_existing)
} EncFrame;

typedef struct {
    EncConfig cfg;
    Rm rm;
    Chan chan;
    int wmb, hmb;
    size_t pitch, lumaSz, chromaSz; // input picture (NVENC input layout); pitch in samples
    size_t rPitch, rLuma, rSlot;    // reconstructed pictures: one slot per DPB entry; rPitch in samples
    size_t bps;                     // bytes per sample: 1, or 2 for 10-bit (the surface pitch fields stay in samples)
    // per frame in flight (slot = ticket % depth): input picture, picture setup, bitstream, status
    RmMem in[ENC_MAX_DEPTH], setup[ENC_MAX_DEPTH], bs[ENC_MAX_DEPTH], status[ENC_MAX_DEPTH];
    struct { bool idr; unsigned frame_num, poc; double t0; int slot, nRefs, refs[ENC_MAX_REFS], refOh[ENC_MAX_REFS], lf; bool b, hidden; } pend[ENC_MAX_DEPTH];
    RmMem recon, hist, coloc, mepred[2], sem, rcproc, rcdata;
    NvU32 seq;                      // frames submitted (semaphore payload of the last one)
    NvU32 waited;                   // frames waited for
    bool forceIdr;                  // after a failed frame
    NvU64 extInput;                 // != 0: GPU address of an input picture in NVENC's layout (luma, chroma at +lumaSz) to use instead of in[]
    unsigned frame_num, sinceIdr;   // frame_num of the next picture, pictures since the last IDR
    int slotOf[ENC_MAX_REFS];       // DPB slots of the reference frames, most recent first
    int nRefs;                      // valid entries in slotOf
    int nextSlot;
    int bSlot;                      // reconstruction slot for B pictures (non-reference)
    unsigned slotPoc[ENC_MAX_REFS + 2];   // POC of the picture in each reconstruction slot
    int idrDisp;                    // display index of the last IDR (enc_submit_ex)
    RmMem colocB;                   // co-located data a B picture writes (the anchor's must survive for the next B)
    // two passes (EncConfig.two_pass): the first pass's picture setups and its scratch outputs (bitstream, status, history,
    // reconstruction, co-located data, motion vectors for the second pass)
    RmMem setup1[ENC_MAX_DEPTH], bs1, status1, hist1, rec1, coloc1, mepred1;
    RmEvent ev;                     // NVENC non-stall event (ev.h == 0: none, spin)
    // AV1: default probability tables, the tables the engine wrote, entropy / filter line buffers, assembled temporal units
    RmMem av1ProbIn, av1Prob, av1Line[6];   // av1Prob: the CDFs each reconstruction slot's frame saved (0x10000 each), then scratch
    int nSlots;                     // reconstruction slots
    // AV1 B pictures (T12.08): an anchor with B pictures before it is coded hidden; its frame OBU goes into the next shown
    // frame's temporal unit, and enc_av1_show_existing() makes the temporal unit that shows it
    uint8_t *av1Hid; size_t av1HidN; int av1HidSlot, av1HidDisp, av1LastShown;
    size_t aqStride;                // bytes per frame of AQ offsets in the RC data (from 0x10000, one area per frame in flight)
    bool aqValid[ENC_MAX_DEPTH];    // the frame in that input slot has AQ offsets
    uint8_t *seiBuf;                // H.264 / HEVC key frames with HDR metadata: the SEI, then the engine's NAL units
    uint8_t *av1Tu[ENC_MAX_DEPTH]; size_t av1TuCap;
    int av1SbCols, av1SbRows;
    bool av1Hq;                     // AV1: the default preset's CDEF (4 strengths) and loop restoration (EncConfig.quality, B pictures)
    int av1TileColsLog2, av1TileRowsLog2, av1TileW, av1TileH, av1TileCols, av1TileRows;   // uniform tile grid (superblocks)
} Enc;

int  enc_open(Enc *e, const EncConfig *cfg);                    // 0 = ok
void enc_close(Enc *e);
size_t enc_headers(Enc *e, uint8_t *out);                       // SPS + PPS (HEVC: VPS + SPS + PPS), Annex B (~64 / ~96 bytes)
size_t enc_av1_config(Enc *e, uint8_t *out);                   // AV1: the sequence header OBU (for av1C; < 32 bytes)
// copy an NV12 frame (pitch-linear planes) into the next frame's input surface (needs enc_inflight() < depth)
void enc_load_nv12(Enc *e, const uint8_t *y, size_t yStride, const uint8_t *uv, size_t uvStride);
// copy a 16-bit 4:2:0 frame (P010-style: 10 bits in the high bits of each 16-bit sample) into a 10-bit session's input
void enc_load_p010(Enc *e, const uint16_t *y, size_t yStride, const uint16_t *uv, size_t uvStride);
// copy a planar 8-bit 4:4:4 frame into a 4:4:4 session's input
void enc_load_yuv444(Enc *e, const uint8_t *y, size_t ys, const uint8_t *u, size_t us, const uint8_t *v, size_t vs);
// copy a biplanar 8-bit 4:4:4 frame (Cb Cr interleaved at full width, CoreVideo 444v / 444f) into a 4:4:4 session's input
void enc_load_nv24(Enc *e, const uint8_t *y, size_t ys, const uint8_t *uv, size_t uvs);
// HEVC Main 4:4:4 10: x444 / xf44 (16-bit samples, 10 bits high; Cb Cr interleaved in the second plane)
void enc_load_x444(Enc *e, const uint16_t *y, size_t ys, const uint16_t *uv, size_t uvs);
// convert an l10r frame (ARGB2101010 LE, ScreenCaptureKit's 10-bit format) into a 10-bit session's input (BT.709 video range)
void enc_load_l10r(Enc *e, const uint8_t *src, size_t stride);
// convert a BGRA frame (BT.709 video range) into the next frame's input surface
void enc_load_bgra(Enc *e, const uint8_t *bgra, size_t stride);
int  enc_encode(Enc *e, bool force_idr, EncFrame *out);         // encode the loaded frame and wait; 0 = ok
int  enc_submit(Enc *e, bool force_idr, NvU32 *ticket);         // start encoding the loaded frame; 0 = ok
// B pictures (H.264, HEVC, AV1): like enc_submit, with the picture's display index and whether it is a B picture. The
// caller submits each anchor (I / P) before the B pictures that precede it in display order: list 0 of a B picture is
// the anchors before it, list 1 the newest anchor. Display indices give the POCs / order hints (EncConfig.bframes > 0).
// AV1: an anchor with B pictures before it comes back hidden (EncFrame.hidden); after its last B picture, write
// enc_av1_show_existing().
int  enc_submit_ex(Enc *e, bool force_idr, bool b, int disp, NvU32 *ticket);
int  enc_encode_ex(Enc *e, bool force_idr, bool b, int disp, EncFrame *out);
int  enc_wait(Enc *e, NvU32 ticket, EncFrame *out);             // wait for a submitted frame (in submission order); 0 = ok
// AV1 with B pictures: the temporal unit that shows the last hidden anchor (temporal delimiter + frame header with
// show_existing_frame), to be written after the B pictures before it; returns its size (< 8 bytes)
size_t enc_av1_show_existing(Enc *e, uint8_t *out);
static inline int enc_inflight(const Enc *e) { return (int)(e->seq - e->waited); }
