# GA10x encoding validation

This record covers the `ga10x1` encoding rollout. See the separate
[ga10x2 H.264 decoding record](NVENCVT-GA10X-H264-VALIDATION.md) for the later
decoder change and its artifact identity.

Tested on 2026-11-01 with an RTX 3080 (10de:2206), macOS 26.7.2 (25G332),
and the integrated NullMoth v2.6.6 driver. This validates one GA102 system,
not the entire Ampere family.

The local artifact is `v0.9.5-ga10x1`, revision `ga10x-encoding-1`, based on
mickys/nvenc v0.9.5 (`1e91845d6f37d8bce671d62c14f4390324da3659`). Two local
builds produced the same unsigned dylib SHA-256:
`665bf6bb37901254e1b48c73e784612e7daea89858fbda2e87746ab61c40e88f`.

## Single-session checks

The test process loaded that exact library and registered it before creating
VideoToolbox sessions. It required `org.nvenc.videoencoder.avc` or
`org.nvenc.videoencoder.hevc`, confirmed the hardware property, checked the
registration/factory symbol origins, and rejected additional NVEncVT images.
`NVENCVT_AMPERE_ENC`, `NVENCVT_ANY_GPU`, and `NVENCVT_ENGINE` were unset.

| Codec | Size | Settings | Frames | Result |
| --- | --- | --- | ---: | --- |
| H.264 and HEVC | 1920x1080 | CQP, no B frames | 30 each | Pass |
| H.264 and HEVC | 1920x1080 | CBR, 6 Mbit/s requested | 30 each | Pass |
| H.264 and HEVC | 1920x1080 | CQP, two B frames | 30 each | Pass |
| H.264 and HEVC | 3840x2160 | CBR, 25 Mbit/s requested | 12 each | Pass |
| HEVC Main10 | 1920x1080 | CQP | 12 | Pass |
| H.264 and HEVC | 1920x1080 | VBR, 6/10 Mbit/s requested | 12 each | Pass |

All 240 submitted frames were returned with valid PTS/DTS and zero callback,
property, or encode errors. Logs showed NVENC0. Separate FFmpeg 7.1 software
decoder processes decoded every frame with error detection enabled and the
expected dimensions. Per-plane PSNR against the source sequence ranged from
49.87 to 52.76 dB for Y, 44.85 to 51.70 dB for U, and 45.48 to 51.17 dB for V.
Each B-frame case decoded as one I, ten P, and nineteen B frames.

The first isolated HEVC attempt stopped before encoding because VideoToolbox
lazily loaded the old installed library. The retry restricted the child
process's `DYLD_LIBRARY_PATH` to the candidate directory and confirmed that
only the candidate was loaded. This was a provenance gate, not a codec failure;
no global environment or driver configuration was changed for those tests.

Main10 was identified by the software decoder as HEVC Main 10/yuv420p10le.
Its input expanded eight-bit samples into ten-bit buffers, so the comparison
after explicit conversion back to eight bits does not establish ten-bit source
precision or HDR behavior. These short synthetic clips also do not establish
long-duration stability, speed, or exact CBR/VBR conformance.

## Concurrent sessions

A separate process created and prepared H.264 and HEVC sessions before
submitting frames, then interleaved eight 1080p frames per session while both
remained alive. Both reported their exact NVEncVT encoder IDs and hardware
mode, opened NVENC0, and returned all eight frames with valid PTS/DTS. Each
stream independently decoded as one IDR and seven P frames. No second NVENC
engine was opened. The GPU fault/reset counters remained clear and the
pre-existing display latch-timeout count remained at two.

## Installed loader and automatic selection

The signed library was installed after preserving the complete previous
bundle. Its SHA-256 is
`1b520968c11c195c8dbc7f29b8c10ccfa543a2eb94c63895477910da4d9f402c`.
Fresh processes initialized the installed Metal plugin using the existing
`NVMTL_NVENCVT=1` configuration. They used neither DYLD overrides nor manual
NVEncVT registration, and their VideoToolbox specification requested hardware
without selecting an encoder ID.

VideoToolbox automatically chose `org.nvenc.videoencoder.avc` and
`org.nvenc.videoencoder.hevc`. Each encoded three 1080p frames on NVENC0 with
hardware mode reported, correct timestamps, and no errors; independent CPU
decoding returned all three frames. The loaded library's path and hash matched
the signed installation. This verifies the real opt-in loader path; it does
not imply that every application initializes Metal early enough to use it.

## Offline checks

`tools/test_nvencvt_ga10x.py` passes four checks covering compiled policy and
class helpers plus the registration/engine/AV1 guards. The staging suite passes
six checks, including refusal of a mismatched local revision without package
mutation. Package-license checks and the real build/sign/manifest staging pass.
After signing, every file-backed Mach-O section of the dylib is identical to
the tested build; the containing Metal driver's changes are confined to its
signature.

## Later HEVC throughput comparison

On the same RTX 3080, the signed `v0.9.5-ga10x2` library (SHA-256
`a5e762b0984e4b8566f4c7d9b4de33f3773461f0a0d085b3191aa1cfb3c20409`)
was compared with the official HEVC encoder. The encoding implementation is
unchanged from ga10x1. Both sessions requested 1920x1080 NV12, 30 fps,
AverageBitRate=8,000,000, RealTime=false and AllowFrameReordering=false.
Eight pre-generated buffers were warmed and drained in the same session,
then replayed for 120 timed submissions plus the final drain. Input creation,
stream writing, CPU decoding and quality calculations were outside the timer.

| Encoder | Run 1 FPS | Run 2 FPS | Run 3 FPS | Median FPS |
| --- | ---: | ---: | ---: | ---: |
| Official NVMTL HEVC | 442.227 | 451.597 | 457.067 | 451.597 |
| Local mickys HEVC | 154.943 | 154.657 | 155.627 | 154.943 |

The order was A/B, B/A, A/B. Each run verified its actual session EncoderID
(`com.nvmtl.videoencoder.hevc.nvenc` or `org.nvenc.videoencoder.hevc`), hardware
flag and installed library provenance, returned all 120 callbacks with matching
source-frame IDs/PTS, and passed independent software decoding of all 120 frames.
Per-frame logging was disabled for the timed runs. CPU-only post-processing was
resumed from retained streams after harness fixes; original encoding windows
were preserved, and each final row records the metric tool hash.

There is **no HEVC encoding throughput gain in this comparison**: the mickys
median is about 34.3% of the official median. This is the same requested VT
settings, not an equal-quality or equal-GOP comparison. Official output was
2,911,649 bytes with two sync/IRAP frames; mickys output was 3,675,311 bytes with
one. Both had no B frames. Their Y/U/V PSNR values were respectively
52.603/49.080/49.077 dB and 53.737/49.973/49.991 dB. The larger mickys stream
also had higher PSNR on this synthetic input. The session covered gaps around
a separate user GUI benchmark, so the machine was not exclusively reserved.
This result does not change the separate H.264 decoding comparison.

The user's later GPU-Z 8/8 run reported 460.1 FPS for HEVC 1080p encoding;
its saved actual EncoderID was the official NVMTL ID above. Software-encoding
scores and explicitly selected official encoder scores do not measure NVEncVT
encoding performance. Apps using automatic selection may pick NVEncVT after
opt-in; use an explicit official EncoderID or the documented
`NVENCVT_AMPERE_ENC=0` fallback when that is the desired encoder path.
