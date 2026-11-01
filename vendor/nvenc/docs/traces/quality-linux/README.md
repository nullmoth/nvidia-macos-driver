# NVIDIA Linux driver: HEVC / H.264 / AV1 quality settings (2026-10-20)

Third tracing session in the Debian VM that owns the RTX 4090 (driver 580.105.08, ffmpeg 7.1), same method as
`docs/traces/bframes-linux` (`trace-linux/nvtrace.so`: RM ioctls, pushed methods, buffers; no disassembly; the user's
decision 2026-10-20 "Tracing session for HEVC quality"). Per run: `log.*.gz`, `out.*`, dumps `dump/execN-710.bin` (setup),
`-70c` (RC data), `-718` (status), `-724` (RC process). Source: `testsrc2`, 1080p30 unless noted.

| run | encode |
|---|---|
| vp5 / vp6 / vp7 | hevc_nvenc `-preset p5 / p6 / p7 -tune hq -rc constqp -qp 28 -bf 0` |
| vp7k | the same as vp7 at 3840x2160 |
| vp7b | vp7 with `-bf 2` |
| vp7c | `-preset p7 -tune hq -rc cbr -b:v 6M -bf 0` |
| vcbr | hevc_nvenc `-preset p4 -rc cbr -b:v 6M -bf 0` |
| vsaq | vcbr + `-spatial_aq 1 -aq-strength 8` |
| vtaq | vcbr + `-temporal_aq 1 -rc-lookahead 8` |
| vla | vcbr + `-rc-lookahead 8` |
| vmp | vcbr + `-multipass fullres` |
| hp7 / hcbr / hsaq | h264_nvenc p7 hq constqp / p4 CBR / p4 CBR + spatial AQ |
| asaq | av1_nvenc p4 CBR + spatial AQ |

Findings (used in `src/enc.c`, T7.08):
- **p6 / p7 use 8x8 coding units** (`min_cu_size` 0; p4 / p5: 16x16), p5+ 4 references in the PPS, ME hint patterns 15 / 63,
  more ME winners, p7 four intra modes compared by SSD, skip evaluation for CU8 / CU64, `rdo_level` 1.
- **CBR rate control** (any preset): `hrd_type` 34, `rhopbi` 286 / 317 / 256, `Bmin` = buffer / 81920 (147 for 12 Mbit),
  `maxQPD` 6, `baseQPD` 3, the buffer twice the bitrate by default.
- **Spatial AQ** = `aqMode` 3 with the driver's map in the RC data at `act_stat_offset`: one signed byte per 32x32 CTB, rows
  256 bytes apart; values about log2(CTB luma variance + 1) - 8.6, clipped at -`aq-strength` (8: from -8 on flat CTBs to +4).
- **Lookahead** (`vla`, T7.09): the driver scales each frame down 4x (480x272, pitch 512, NV12) with a CUDA kernel (CUDA
  memory imported into the encoder's RM client; not traced) and encodes it as an **H.264 P picture at constant QP 40**
  (codec type 3, control 0x00005003 = GPTIMER + DUMP_CYCLE_COUNT, profile 100, CAVLC, one reference = the previous
  scaled frame at temporal distance 1, IDR for the first). No RC data, a discarded bitstream; the driver reads the status
  (`nvenc_pic_stat_s`: `total_intra_cost` / `total_inter_cost`, intra / inter MB counts, bits). The HEVC encode, `lookahead`
  frames behind, differs from plain CBR (`vcbr`) **only in two RC fields**: `ab_beta` ("lookahead info1: ratio of inter over
  intra complexity", 100 on the IDR, then 130, 129, 125, 125, 124, 121) and `prev_act` ("nextI, depth and valid flag": 0x300,
  then 0x200 / 0x100 / 0 as the stream end leaves fewer frames ahead). The RC data are byte-identical. `ab_beta` fits the
  mean of `total_inter_cost / total_intra_cost` over frames N..N+7 times ~415 within +-1 (6 values, not exact).
- **Temporal AQ** (`vtaq`): the same lookahead passes; the encode has `aqMode` 3, `hrd_type` 2 (not 34) and a QP offset map in
  the RC data at `act_stat_offset` (the spatial-AQ layout) from the second frame on: -1 to -3 on the CTBs whose content stays
  (testsrc2's static grid marks), 0 elsewhere - MB-tree-like propagation, computed from the lookahead passes' motion data
  (their `OUT_MEPRED_DATA`, not dumped), so not reproducible from this trace.
- **Multipass fullres** (`vmp`): two executes per frame on the same RC data and RC process buffer. Pass 1: control
  0x12081104 (`RCSTAT_WRITE`), `two_pass_rc` 1, its own setup (cheaper mode decision: its MD block, no transform skip, zero
  quantizer cost vectors, no SAO, FPP partition decision, one ME winner), status (2.3 MB), bitstream, history and
  reconstruction buffers, motion vectors out to a buffer pass 2 reads; it writes `nvenc_rc_pic_stat_s` + row statistics at
  the start of the RC data. Pass 2: 0x12101104 (`RCSTAT_READ`), `two_pass_rc` 2, the normal setup except the ME predictor
  sources (co-located stamps = pass 1's vectors) and search shapes; the real reconstruction and MV buffers.
- The first-pass status dumps of `vmp` (2+ MB each) are left out except `exec1-718.bin`. The tracer dumps some buffers after the
  engine ran (status, RC data), so input / output roles come from the methods, not from the dump contents.
