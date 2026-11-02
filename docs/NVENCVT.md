# Optional NVEncVT VideoToolbox component

The package can include `libnvencvt.dylib`, the VideoToolbox layer from the
pinned `mickys/nvenc` v0.9.5 source with local GA10x codec changes
(`v0.9.5-ga10x2`, revision `ga10x-codecs-2`). It provides NVIDIA NVENC encoders and
NVDEC decoders through VideoToolbox. The library is present in the bundle but
registration is **off by default**.
This page describes the local integration. The vendored README and GA10x notes
remain the upstream v0.9.5 snapshot, including its older decoder-only GA10x policy.

## RTX 30 series encoding

In normal operation, once the optional library is loaded, H.264 and HEVC encoders register on GA10x
only when the NVIDIA RM reports the C7B7 encoder class. A PCI family match alone
is insufficient. Each session uses NVENC0; the RTX 3080 has one NVENC engine.
AV1 encoding remains limited to Ada. NVIDIA's [support matrix](https://developer.nvidia.com/video-encode-decode-support-matrix)
lists the hardware capabilities; it does not establish validation for this port.

For a decoder-only fallback on GA10x, set `NVENCVT_AMPERE_ENC=0` in the target
process environment or in a root-owned `/usr/local/etc/nvencvt.conf` that is not
writable by group or other users. Restart the target app. Unset or `1` enables
the GA10x encoders after the class check; other values disable them.
`NVENCVT_ENGINE=1` is rejected on GA10x; leave it unset or use `0`.
The upstream `NVENCVT_ANY_GPU` development override bypasses GPU selection;
leave it unset for normal operation and hardware validation.
See the [RTX 3080 validation record](NVENCVT-GA10X-VALIDATION.md) for tested
formats, source identity, and the limits of the hardware checks.

## RTX 30 series H.264 decoding

The local GA10x revision also registers H.264 decoding when RM reports the
C7B0 NVDEC class. The `avc1` and `avc3` registrations use the existing H.264
implementation with explicit C7B0/C9B0 interface checks. This path accepts
eight-bit 4:2:0 video with coded dimensions at most 4096x4096; it does not
support FMO/slice groups or SP/SI slices.

`NVENCVT_AMPERE_H264_DEC=0` keeps the previous H.264 decoder selection without
disabling the NVEncVT encoders or VP9/AV1 decoders. Unset or `1` enables the
H.264 registration after the class check; other values disable it. This
specific setting takes precedence over the upstream `NVENCVT_AMPERE_ALL_DEC`
development override. HEVC and MPEG decoding retain their earlier GA10x gate;
normal use should leave that broad override unset.
The [H.264 validation record](NVENCVT-GA10X-H264-VALIDATION.md) documents the
exact decoder attribution, CPU reference checks and fixed-input comparison.

## Apps that load the Metal plugin

To opt in, add one of these lines to the personal settings block in
`/Library/GPUBundles/nvmtl/nvrm610.conf`:

```text
NVMTL_NVENCVT=OBS,SomeOtherApp
```

Each selector must match a process name returned by `getprogname()`. Separate
names with commas, spaces, or colons; the installer keeps these selectors
across updates up to the loader's 511-byte limit. `NVMTL_NVENCVT=1` opts every
eligible process in; leave the setting unset for the default-off behavior.
`WindowServer` is always excluded. Restart the target app after changing the
setting.

The library is loaded only when the app's process initializes NullMoth's Metal
device. The loader accepts it only from a regular, root-owned file inside a
root-owned directory that is not writable by group or other users. The package
installer applies those ownership and permission rules to the whole bundle.

This hook registers inside the process that initializes Metal. It does not
change Apple's VideoToolbox service or sandbox behavior. Upstream's follow-up
documentation at [mickys/nvenc commit 90d1d6a](https://github.com/mickys/nvenc/blob/90d1d6a/docs/benchmarks.md#L550-L560)
records QuickTime using its own process and explains that Safari's WebKit GPU
process cannot map the driver plugin under its sandbox, limiting NVEncVT use
there, particularly for hardware decoding. This package does not change that
Safari limitation or claim Safari hardware decoding. Those are upstream
findings, not validation on every supported system.

## Command-line tools

For a command-line tool such as `ffmpeg`, the first VideoToolbox session can be
created before the client asks for a GPU device and loads the Metal plugin. That
session may snapshot the decoder list too early. Preload the library into the
command's process so its registrar runs before the first session:

```sh
DYLD_INSERT_LIBRARIES=/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib \
  ffmpeg -i input.mov -c:v hevc_videotoolbox -b:v 20M output.mp4

DYLD_INSERT_LIBRARIES=/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib \
  ffmpeg -hwaccel videotoolbox -i input.mkv -f null -
```

No `nvenc-run` wrapper is included in this driver package. The upstream source
notes that injecting before process startup also lets VideoToolbox see the
decoders before a command-line client's first session enumerates its decoder
list.

## Firefox (VP9 and AV1)

On Ada, Firefox decodes VP9 and AV1 (and H.264 / HEVC) on NVDEC through
NVEncVT. Measured on an RTX 4060 Ti (AD106), macOS 26.7.2, Firefox 159, a
YouTube 4K60 video: Firefox's media log reports `VP9 SWDEC HWDEC, AV1 SWDEC
HWDEC`, the media process uses 12-17% of a core instead of 90-350%, and
"GPU Video Decode Utilization" reads 9-67%.

`NVMTL_NVENCVT=plugin-container` alone is not enough. Firefox decodes in
`plugin-container` and asks VideoToolbox once, when that process starts, which
codecs have hardware decoders; VideoToolbox answers from a list it built before
the Metal plugin loaded NVEncVT, so the first answer for `vp09` is no. The
library has to be in the process from its start, as for command-line tools.
Preloading `libnvencvt.dylib` itself reaches every Firefox process, and
Firefox's crash helper, forked while VideoToolbox's start-up threads held an
allocator lock, crashed at every start. Firefox is therefore started with a
small loader that links only libSystem and opens the library only in the
processes `NVENCVT_INJECT_ONLY` names:

```sh
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -O2 -Wall -dynamiclib \
  -o /tmp/libnvencvt-loader.dylib tools/firefox-nvdec/nvencvt-loader.c
sudo install -o root -g wheel -m 755 /tmp/libnvencvt-loader.dylib \
  /Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/
tools/firefox-nvdec/firefox-nvdec.sh
```

Quit Firefox before running the script. A driver update replaces the bundle
and removes the loader; install it again afterwards. Firefox's runtime does not
carry the `allow-dyld-environment-variables` entitlement: the preload works
because the driver's AMFI boot arguments are set.

Firefox's sandboxes refuse `/dev/null`. NVEncVT's RM connection and the
plugin's VRAM import of app surfaces both fall back to an empty pipe there
(`nvmtl_icd_open`); without it the decoder could not start and every Firefox
window surface was read back in full after each frame.

Settings read from the environment:

- `NVENCVT_INJECT_ONLY=<names>`: when the library is preloaded, register only
  in these processes (comma, colon or space separated). Unset: every process.
- `NVDEC_HOLD=<0..16>` (default 8): pictures kept referenced after they are
  handed to the app. A client in another process (Firefox's GPU process) takes
  over the surface a few milliseconds after the decoder lets it go; without the
  hold the pool reused it first and older or half-written pictures showed.
- `NVENCVT_LOG=2`: one log line per decoded VP9 / AV1 frame with the time and
  NVDEC's status.

The `/tmp` flag files of the upstream diagnostics (`/tmp/nvencvt.log`,
`.refs`, `.dpb`, `.stats`, `.dump`, `.barcode`, `.anyfmt`) and the renaming of
VideoToolbox's registry entries are compiled only with
`-DNVENCVT_TMP_DIAGNOSTICS=1` and `-DNVENCVT_REGISTRY_RENAME=1`: `/tmp` is
writable by every account.

## Building a release package

On the build Mac, build the local x86_64 dylib and then stage it into an already
assembled package root:

```sh
build/nvenc_build.sh /tmp/nvencvt-build
package/stage_nvencvt.py /path/to/pkgroot /tmp/nvencvt-build
```

The staging command checks both the upstream pin and exact local artifact
version/revision in the schema v2 build receipt, plus the unsigned build hash,
copies the upstream license texts and third-party notices beside the library
and into the package license folder, signs the dylib and containing bundle, then regenerates
and verifies `SHA256SUMS` for the final signed package. The build receipt kept
in the bundle records the pre-sign build hash; `SHA256SUMS` covers the final
signed bytes. Staging does not install or load the library.
