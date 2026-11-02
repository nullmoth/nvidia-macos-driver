# nvenc

Hardware video encoders and decoders for macOS on NVIDIA RTX 40 (Ada) GPUs and the GTX 1650 (TU117), running on
[NullMoth's NVIDIA driver](https://github.com/nullmoth/nvidia-macos-driver) and offered to apps through VideoToolbox.
It drives the GPU's NVENC and NVDEC engines directly from user space (an RM client, GPFIFO channels and NVIDIA's
documented picture setups), with no closed codec library in between.

| | Codecs |
|---|---|
| **Encode** (NVENC) | H.264 (Main / High, CABAC, 4:4:4, lossless, up to 8K), HEVC (Main, Main 10, 4:4:4, lossless), AV1 (8 / 10-bit, tiles up to 8K) |
| **Decode** (NVDEC) | H.264, HEVC (8-12 bit, 4:2:0 / 4:4:4), VP9 (8-12 bit), AV1 (8 / 10-bit, film grain), MPEG-1/2, MPEG-4 Part 2 through VideoToolbox; VP8 and VC-1 in the library and tools only (nothing on macOS asks VideoToolbox for them) |
On the GTX 1650 (TU117, the only Turing card tested; other RTX 20 cards keep NullMoth's encoders): no AV1 (the hardware has
none), HEVC encodes without B pictures (NVENC 6.6 has none), VP9 decodes from 65x65 up; details in
[docs/tasks.md](docs/tasks.md) M15.

On the RTX 30 (GA10x, consumer Ampere: the RTX 3060 Ti among them) the **VP9 and AV1 decoders** register, and that is the
point: the NVDEC there (class 0xc7b0) decodes AV1 in hardware like Ada's, with the same method IDs, while its NVENC is
neither Ada's 8.2 nor TU117's 6.6, so our encoder setups do not apply and NullMoth's own encoders keep working for these
cards. 4K works too: the decoder hands the pictures out of its own pool, made with the client's destination attributes,
so VideoToolbox passes them on instead of transferring every 4K frame (15 ms -> 0.05 ms per frame). How it was ported,
what it measures and the 4K finding: [docs/ga10x.md](docs/ga10x.md).

Encoders: B pictures (two between anchors; real-time sessions stay P-only), CBR / VBR with NVIDIA's rate-control
settings, quality presets taken from NVIDIA's own driver where the frame rate allows, colour description and HDR10
metadata, real-time latency of about 2 ms per 1080p and 5-6 ms per 4K frame. Decoders: bit-exact against ffmpeg on the
conformance suites (details per codec in [docs/tasks.md](docs/tasks.md)); 4K H.264 at ~190-210 fps, HEVC / VP9 / AV1 at
~300-380 fps.

**Benchmarks** against Apple's, NullMoth's and ffmpeg's codecs, with usage examples and a findings log:
[docs/benchmarks.md](docs/benchmarks.md). In short: our AV1 is the best quality of every encoder tested at 10x x265's
speed, HEVC is level with x265 on VMAF, H.264 at or above x264 medium; the decoders are 4-5x NullMoth's for H.264 and
the only hardware VP9 / AV1 decoders on an Intel Mac.

## Using it

Releases: [github.com/mickys/nvenc/releases](https://github.com/mickys/nvenc/releases) (since 0.9.5: `libnvencvt.dylib` built
from the tag, unsigned, with LICENSE, this README and SHA256SUMS). The easy way to install it is the
[mickys overlay](https://github.com/mickys/nvidia-macos-driver/releases) for NullMoth's driver, which ships the same library
and sets everything below up. By hand, with NullMoth's driver 2.3.7 or later:

```sh
sudo install -o root -g wheel -m 755 libnvencvt.dylib /Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/
sudo codesign -f -s - /Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib
echo NVMTL_NVENCVT=1 | sudo tee -a /Library/GPUBundles/nvmtl/nvrm610.conf   # NullMoth's plugin then loads it in every app
DYLD_INSERT_LIBRARIES=/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib ffmpeg ...   # command-line tools
```

- **Apps** (Parsec, OBS, QuickTime and other AVFoundation apps): NullMoth's Metal plugin loads
  `libnvencvt.dylib` into every process that uses Metal when `NVMTL_NVENCVT=1` is in `nvrm610.conf` (its own hook since
  driver 2.3.7; the library must be root-owned). VideoToolbox then lists "NVENC - H.264 - (NVIDIA <card>) - mickys/nvenc -
  <version>", its HEVC and AV1 twins, and our decoders, rated above Apple's and NullMoth's.
- **Command-line tools** (ffmpeg, HandBrakeCLI, avconvert) never use Metal, so they get the codecs only through the
  overlay's wrapper: `nvenc-run ffmpeg -i in.mov -c:v hevc_videotoolbox -b:v 20M out.mp4` (it sets `DYLD_INSERT_LIBRARIES`
  for that one tool; the library registers itself when injected). macOS has no supported way for a third party to
  register a video encoder for every process ([docs/reports/T7.00-cli-and-extensions.md](docs/reports/T7.00-cli-and-extensions.md)).
  Decoding needs it too: a plain `ffmpeg -hwaccel videotoolbox` gets our decoders only from its second VideoToolbox session
  (the first copies the decoder list before it starts the GPU, which is what loads the plugin and registers ours) and drops
  the first packet with it: for AV1 everything up to the next key frame is missing (dropped as "reference missing").
  `nvenc-run` registers them before the first session. The overlay's `--cli-wrappers` puts wrappers named `ffmpeg`,
  `ffprobe`, `ffplay`, `HandBrakeCLI` and `avconvert` first in `PATH` so that plain commands and scripts go through it.
- **Settings:** environment variables, or system-wide in `/usr/local/etc/nvencvt.conf` (lines `NAME=VALUE`, root-owned
  and not writable by others; the environment wins). The useful ones:

  | Setting | Effect |
  |---|---|
  | `NVENCVT_NO_DEC=1` | no decoders (only the encoders); `NVENCVT_NO_HEVCDEC`, `_VP9DEC`, `_AV1DEC`, `_MPEG2DEC`, `_MPEG4DEC` = one of them |
  | `NVENCVT_NO_HEVC=1`, `NVENCVT_NO_AV1=1` | no HEVC / AV1 encoder |
  | `NVENCVT_BFRAMES=0..3` | B pictures between anchors when the app allows reordering (default 2) |
  | `NVENCVT_FAST=1` | the fast encoder controls even when not real time |
  | `NVENCVT_TWOPASS=1` | NVIDIA's two-pass mode (H.264 / HEVC with a bitrate; small gain for 2x / +7 % engine time) |
  | `NVDEC_SYNC=1` | decoders always synchronous |
  | `NVENCVT_LOG=1` | log to stderr (or create `/tmp/nvencvt.log`: one log per process in `/tmp`) |

  Encoder interface and VideoToolbox properties: [docs/vt-encoder.md](docs/vt-encoder.md).

## Layout

| Path | What |
|---|---|
| `src/rm.c`, `channel.c` | RM client (memory, GPU / CPU mappings, events) and GPFIFO channels on an engine |
| `src/enc.c`, `*hdr.c`, `*enctmpl.h`, `la.c` | the encode session for H.264 / HEVC / AV1 (references, B pictures, rate control, two-pass), parameter sets / OBUs, NVIDIA's control blocks, lookahead analysis |
| `src/dec.c`, `hevcdec.c`, `vp9dec.c`, `av1dec.c`, `mpeg2dec.c`, `mpeg4dec.c`, `vp8dec.c`, `vc1dec.c` (+ `*parse.c`) | the decoders: bitstream parsers, DPB management, NVDEC picture setups |
| `src/blocklinear.c` | NVENC / NVDEC surface layouts (SIMD tiling and untiling) |
| `vt/` | `libnvencvt.dylib`: the VideoToolbox encoders (`nvencvt.c`) and decoders (`nvencvt_dec.c`) |
| `tools/` | test programs: `encodeN`, `vtencfile` (encode), `*decodeN`, `vtdecode` (decode), `probe`, `hello`, setup dumpers, VideoToolbox probes |
| `trace/`, `trace-linux/` | runtime tracers (RM escapes, channel methods, buffers) used to observe working clients |
| `scripts/` | template generators, trace tools, `guest/` benchmark and conformance runners, `bench-table.py` |
| `docs/` | `design.md` (sources, design, lessons), `tasks.md` + `reports/` (every task and what it found), `benchmarks.md` + `bench/`, `traces/` |

## Build and run

Needs a macOS machine (or guest) with NullMoth's driver and the Command Line Tools, reachable over ssh.

```sh
scripts/fetch-vendor.sh    # NVIDIA open-gpu-kernel-modules (RM API, 610.57.04) and open-gpu-doc into vendor/
./build.sh                 # NVENC_GUEST=<ssh host>, default "tahoe"; binaries land in the guest's ~/nvenc-dev
ssh tahoe ~/nvenc-dev/probe
ssh tahoe 'cd ~/nvenc-dev && SRC=clip.nv12 RC=2 BITRATE=8000000 ./encodeN -hevc 1920 1080 300 28 30 1 /tmp/o.hevc /dev/null'
```

Releases: `scripts/release-package.sh <tag> <out>` (clean tree, annotated tag on HEAD made locally, pushed only when the
release is published) builds the library in the guest and packs it; the release goes up as draft -> upload -> digest check ->
publish (GitHub releases here are immutable: a published tag stays).

## Sources

NVIDIA's open-gpu-doc (`nvenc_drv.h`, `nvdec_drv.h`, class headers) and open-gpu-kernel-modules, the codec standards,
Apple's published headers and SPI declarations, and NullMoth's open-source code. Where the documents leave something
out, it was found by experiment or by observing working clients at run time (their RM calls, pushed methods and
buffers): NullMoth's H.264 / HEVC encoder libraries on macOS and NVIDIA's Linux driver (AV1 encode, B pictures, quality
presets, rate control). Two open-source decoders were read for facts the headers lack: mesa-nova (MIT; AV1 decode) and
averne's FFmpeg `nvtegra` (VP8 / VC-1 decode); no code was taken. No disassembly or patching of closed binaries, and the
decoders were never traced from NullMoth's libraries.

## License

MIT (see `LICENSE`). The headers in `vendor/` (NVIDIA open-gpu-kernel-modules, open-gpu-doc, WebKit SPI) keep their own
licences.
