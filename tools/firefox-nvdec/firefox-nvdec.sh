#!/bin/sh
# Starts Firefox with NVEncVT's VP9 / AV1 / HEVC / H.264 decoders (docs/NVENCVT.md, "Firefox"). Firefox asks VideoToolbox
# for hardware decoders once, when plugin-container starts and before the Metal plugin would load NVEncVT, so the loader
# is preloaded instead. Quit Firefox first: an open Firefox is reused.
LOADER=/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt-loader.dylib
[ -f "$LOADER" ] || { echo "firefox-nvdec: $LOADER is missing (docs/NVENCVT.md, Firefox)" >&2; exit 1; }
exec open -a Firefox --env DYLD_INSERT_LIBRARIES="$LOADER" --env NVENCVT_INJECT_ONLY=plugin-container "$@"
