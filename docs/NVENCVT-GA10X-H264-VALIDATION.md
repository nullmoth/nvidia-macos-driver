# GA10x H.264 decoding validation

Tested on 2026-11-01 with an RTX 3080 (10de:2206, GA102, NVDEC C7B0),
macOS 26.7.2 (25G332), and the integrated NullMoth v2.6.6 driver.
The artifact is `v0.9.5-ga10x2`, local revision `ga10x-codecs-2`, based on
mickys/nvenc v0.9.5. Its unsigned dylib SHA-256 is
`5fc71b76ef5a9fbb7cfbf7e11b91f444c32721d2e2d6fdcc0531e795baec0317`.

The earlier GA10x policy returned `kVTCouldNotFindVideoDecoderErr` without
calling H.264 registration. The local change registers H.264 only after
confirming C7B0, independently of the still-restricted HEVC/MPEG decoders.
The H.264 GPU command sequence is unchanged. Compile-time checks cover every
used C7B0/C9B0 method and field, including all 17 reference-surface offsets.

## Attribution and correctness

The candidate was loaded in a separate test process with both
`NVENCVT_AMPERE_ALL_DEC` and `NVENCVT_AMPERE_H264_DEC` unset. A process-local
registration shim recorded the decoder factory VideoToolbox actually called,
its implementation ID, library path, SHA-256, successful result and instance.
Registration alone and `UsingHardwareAcceleratedVideoDecoder` alone were not
accepted as proof of decoder identity.

The official path called `com.nvmtl.videodecoder.avc.nvdec` from
`libnvdec_h264_t2d.dylib` (SHA-256
`fdfd6e9c356bb4eac42d6841d950db3545cb97e6bcb2b039132d190119a52de3`).
The candidate called `org.nvenc.videodecoder.avc` from the exact build above.
Both returned all 128 frames of the fixed 1080p H.264 stream with hardware
mode reported and every active Y/UV byte equal to an independent FFmpeg 7.1
software reference.

The input MP4 SHA-256 is
`58c9e462006e663dfb6cd48585c0bc2fff67f1c1e82d31927df0608ec3e2aab1`;
the CPU I420 reference SHA-256 is
`268b8a469e92fe274fd637cc117ade495ce3cf2880025e133310e99252ef396d`.
The stream includes two B frames between reference frames and closed GOPs.
Each callback was matched to its submitted frame and exact
`CMSampleBufferGetOutputPresentationTimeStamp`, including the MP4 edit-list
offset. Callbacks were not required to arrive in display order, as documented
by [Apple](https://developer.apple.com/documentation/videotoolbox/vtdecompressionoutputcallback).
Pixel comparisons used presentation order; callback records were collected
under a mutex.

## Fixed-input performance comparison

Each fresh process warmed samples 0..7 in one decompression session, drained
asynchronous output, reset its counters, then replayed samples 0..119 in that
same session. This follows the CodecBench decode timing window. Both paths
used asynchronous decoding and NV12 output. Source reading, source hashes,
session creation and pixel comparisons were outside the timed interval.
Every measured run returned 120 images with hardware mode reported and zero
errors; every run also verified the actual decoder factory.

| Decoder | Run 1 FPS | Run 2 FPS | Run 3 FPS | Median FPS |
| --- | ---: | ---: | ---: | ---: |
| Official NVMTL H.264 | 221.0 | 218.4 | 221.2 | 221.0 |
| Local mickys C7B0 H.264 | 683.9 | 687.7 | 683.0 | 683.9 |

Runs alternated A/B, B/A, A/B. The median ratio is 3.09x (+209.46%) for this
fixed synthetic stream. It is not a prediction for every video, resolution or
application, and these short tests do not establish long-duration stability.

## Installed automatic selection

The signed ga10x2 library was installed into the existing Metal bundle after
backing up and validating the complete previous bundle. Its installed SHA-256
is `a5e762b0984e4b8566f4c7d9b4de33f3773461f0a0d085b3191aa1cfb3c20409`.
All 43 bundle paths matched the staged manifest, ownership and permissions
passed, and `codesign --verify --deep --strict` passed. The driver configuration
and kernel extensions were unchanged.

A fresh process initialized the real Metal device with the existing
`NVMTL_NVENCVT=1` configuration. It used no DYLD overrides, manual registrar
call, decoder selector or Ampere override. The installed library's actual
`avc1` instance returned three hardware-decoded 256x128 frames with matching
timestamps, zero decoder errors and full Y/UV equality to the CPU reference.
The only diagnostic environment setting was `NVENCVT_LOG=1`.

Library observation used `RTLD_NOLOAD`, dyld image enumeration and `dladdr`
before and after the session to verify the unique installed image and hash.
The first harness attempt stopped before session creation because Metal loads
the symbols locally; `RTLD_DEFAULT` could not resolve them. The corrected
observation used the already-loaded library handle without loading it again.
This is a short automatic-selection smoke test; the 1080p performance numbers
above come from the separately attributed A/B processes.

## Additional format checks

- 256x128 High profile: single IDR and IDR plus two P frames, full-plane CPU
  equality. The new default registration also passed the three-frame case.
- 640x360 Baseline/CAVLC `avc1`: one IDR plus 15 P frames, 16/16 bit-exact.
- The same 640x360 Baseline/CAVLC stream with an `avc3` sample entry:
  16/16 bit-exact with the default C7B0 registration.
- 3840x2160 High profile `avc1`: one IDR plus 11 P frames, 12/12 bit-exact.

The pre-existing display latch-timeout count remained at two; GPU surface
timeout and IOP failure counters remained zero through these checks. The
local policy can be disabled with `NVENCVT_AMPERE_H264_DEC=0` without disabling
the NVEncVT encoders or VP9/AV1 decoders.
