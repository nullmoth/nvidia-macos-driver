#!/bin/sh
# Starts Firefox with NVEncVT preloaded. Not needed when nvrm610.conf names plugin-container in NVMTL_NVENCVT
# (docs/NVENCVT.md, "Firefox"); a fallback for when about:support still shows no VP9 / AV1 hardware decoding.
# Quit Firefox first: an open Firefox is reused.
LOADER=/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt-loader.dylib
[ -f "$LOADER" ] || { echo "firefox-nvdec: $LOADER is missing (docs/NVENCVT.md, Firefox)" >&2; exit 1; }
exec open -a Firefox --env DYLD_INSERT_LIBRARIES="$LOADER" --env NVENCVT_INJECT_ONLY=plugin-container "$@"
