#!/bin/bash
# stage_vulkan_loader.sh <pkgroot> <loader out dir from build/build_vulkan_loader.sh>: put the Khronos Vulkan loader, its licence
# and the opt-in script into a package tree. Nothing installs it by default; the user runs
#   sudo bash vulkan-for-apps.sh --install --with-loader Library/NullMoth/vulkan/libvulkan.1.dylib
set -euo pipefail
ROOT=${1:?pkgroot}; SRC=${2:?loader out dir}
HERE=$(cd "$(dirname "$0")" && pwd)
lipo -archs "$SRC/libvulkan.1.dylib" | grep -qx x86_64 || { echo "STOP: the loader is not x86_64"; exit 1; }
mkdir -p "$ROOT/Library/NullMoth/vulkan"
cp "$SRC/libvulkan.1.dylib" "$ROOT/Library/NullMoth/vulkan/"
cp "$HERE/../licenses/Vulkan-Loader-LICENSE.txt" "$ROOT/Library/NullMoth/vulkan/LICENSE.txt"
cp "$HERE/vulkan-for-apps.sh" "$ROOT/Library/NullMoth/vulkan/"
echo "staged the Vulkan loader $(shasum -a 256 "$ROOT/Library/NullMoth/vulkan/libvulkan.1.dylib" | cut -c1-8)"
