# Upstream source and omitted trace data

This directory vendors the complete production source, build script, tools, trace interposers, notes, and text documentation from `mickys/nvenc` tag `v0.9.5` (commit `1e91845d6f37d8bce671d62c14f4390324da3659`, tree `2517cf69cc0edd8e9925ad9c443ed1098ded95a4`). The only omitted upstream files are 1,279 non-text capture/trace samples below `docs/traces/`, totaling 93,991,779 bytes. `UPSTREAM-OMITTED-DATA.json` records every omitted path, upstream Git blob SHA-1, and size. The text readmes, case notes, scripts, and source readers for those samples remain included.

To fetch the exact full upstream tree, including the omitted samples:

```sh
git clone --filter=blob:none --no-checkout https://github.com/mickys/nvenc.git nvenc-v0.9.5
git -C nvenc-v0.9.5 fetch --depth=1 origin refs/tags/v0.9.5:refs/tags/v0.9.5
test "$(git -C nvenc-v0.9.5 rev-parse 'refs/tags/v0.9.5^{commit}')" = 1e91845d6f37d8bce671d62c14f4390324da3659
git -C nvenc-v0.9.5 checkout --detach 1e91845d6f37d8bce671d62c14f4390324da3659
```

The pinned implementation dependencies needed by the local build are included under `vendor/`: NVIDIA open-gpu-kernel-modules header trees at release 610.57.04, NVIDIA open-gpu-doc video class headers, and the two WebKit VideoToolbox SPI headers. No kernel module source is built or linked. The local build wrapper is `build/nvenc_build.sh`; run `build/nvenc_build.sh <output-directory>` from the driver checkout. It compiles and links `libnvencvt.dylib` locally and writes `nvencvt-build-info.json` (upstream pin, x86_64 architecture, minimum OS, SDK, and pre-sign SHA-256) plus the linker log. It performs no signing, installation, library loading, or runtime test. All build headers are vendored, so this wrapper does not fetch network data. Upstream `build.sh` remains unchanged for its full original tool-building workflow, which expects a macOS build host and can use its optional SSH target.
