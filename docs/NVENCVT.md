# Optional NVEncVT VideoToolbox component

The package can include `libnvencvt.dylib`, the VideoToolbox layer from the
pinned `mickys/nvenc` v0.9.5 source. It provides NVIDIA NVENC encoders and
NVDEC decoders through VideoToolbox. The library is present in the bundle but
registration is **off by default**.

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

The staging command checks the pinned build metadata and unsigned build hash,
copies the upstream license texts and third-party notices beside the library
and into the package license folder, signs the dylib and containing bundle, then regenerates
and verifies `SHA256SUMS` for the final signed package. The build receipt kept
in the bundle records the pre-sign build hash; `SHA256SUMS` covers the final
signed bytes. Staging does not install or load the library.
