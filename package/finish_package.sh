#!/bin/bash
# finish_package.sh <pkgroot> --nvencvt <libnvencvt.dylib> --nmsmn <NMSMN.kext> <ryzenadj> <sha256>
#                   [--boost <nvmtl-boost>] [--vulkan-loader <loader out dir>]
# The package build's last stage, in one fixed order: the video codec library, the Ryzen power parts, the optional boost
# daemon and Vulkan loader, then sums_complete.sh, then check_package.sh. Each staging script refreshes its own SHA256SUMS
# lines; nothing here rewrites the whole list, so a file the base tree carries without a line still fails the check instead
# of being blessed. Stops at the first failure. The tree's install.sh must be this folder's install.sh (a stale installer
# would not run the completeness check).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=${1:?usage: finish_package.sh <pkgroot> --nvencvt <lib> --nmsmn <kext> <ryzenadj> <sha256> [--boost <bin>] [--vulkan-loader <dir>]}
shift
NVENC=; NKEXT=; NBIN=; NSHA=; BOOST=; VLOAD=
while [ $# -gt 0 ]; do
  case "$1" in
    --nvencvt) NVENC=${2:?}; shift 2;;
    --nmsmn) NKEXT=${2:?}; NBIN=${3:?}; NSHA=${4:?}; shift 4;;
    --boost) BOOST=${2:?}; shift 2;;
    --vulkan-loader) VLOAD=${2:?}; shift 2;;
    *) echo "finish_package: unknown argument $1" >&2; exit 2;;
  esac
done
[ -n "$NVENC" ] || { echo "finish_package: --nvencvt is required" >&2; exit 2; }
[ -n "$NKEXT" ] || { echo "finish_package: --nmsmn is required" >&2; exit 2; }
[ -f "$ROOT/SHA256SUMS" ] || { echo "finish_package: $ROOT has no SHA256SUMS (the base tree is not built)" >&2; exit 1; }
cmp -s "$ROOT/install.sh" "$HERE/install.sh" || { echo "finish_package: $ROOT/install.sh is not package/install.sh" >&2; exit 1; }
bash "$HERE/stage_nvencvt.sh" "$ROOT" "$NVENC"
bash "$HERE/stage_nmsmn.sh" "$NKEXT" "$NBIN" "$NSHA" "$ROOT"
[ -z "$BOOST" ] || bash "$HERE/stage_boost.sh" "$ROOT" "$BOOST"
[ -z "$VLOAD" ] || bash "$HERE/stage_vulkan_loader.sh" "$ROOT" "$VLOAD"
cp "$HERE/sums_complete.sh" "$ROOT/sums_complete.sh"
bash "$HERE/sums_refresh.sh" "$ROOT" sums_complete.sh
bash "$HERE/check_package.sh" "$ROOT"
