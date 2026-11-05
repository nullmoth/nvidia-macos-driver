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
