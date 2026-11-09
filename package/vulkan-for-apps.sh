#!/bin/bash
# vulkan-for-apps.sh: make the driver's Vulkan implementation (NVK) visible to macOS applications that use the standard
# Vulkan loader. Off by default; this script is the switch.
#
#   sudo bash vulkan-for-apps.sh --install [--with-loader <libvulkan.1.dylib>]
#                                              write the loader manifest; with the loader, also install it
#   sudo bash vulkan-for-apps.sh --remove      delete it
#        bash vulkan-for-apps.sh --status      say whether it is installed and whether its library exists
#
# What it does: writes one loader manifest, <prefix>/share/vulkan/icd.d/nullmoth_nvk_icd.json, pointing at the NVK library
# the driver already installed under /Library/GPUBundles/nvmtl. The Vulkan loader finds manifests in that folder on its own
# (/usr/local/share/vulkan/icd.d is on its default search path), so no environment variable is needed. It changes nothing
# else and never touches the Metal path: WindowServer and Metal applications keep using the driver exactly as before.
#
# The Vulkan loader is the Khronos Vulkan-Loader (Apache-2.0, which allows redistribution with its licence text and notice kept;
# see NOTICE). A package that carries an x86_64 build of it passes it with --with-loader and it is installed to
# <prefix>/lib/libvulkan.1.dylib; without that option a loader already on the Mac (LunarG SDK, Homebrew vulkan-loader) is used.
# Windows: the driver's NVK exposes VK_EXT_metal_surface (nvk/nvk-metal-wsi.patch) and presents through the CAMetalLayer
# helper in Mesa's Metal swapchain. Frames reach the layer through a host-visible copy, so it is correct but not the
# fastest path; compute and off-screen rendering are unaffected.
set -euo pipefail
PREFIX=${NULLMOTH_VK_PREFIX:-/usr/local}
LIBDIR=${NULLMOTH_VK_LIBDIR:-/Library/GPUBundles/nvmtl}
LIB=$LIBDIR/libvulkan_nouveau.dylib
DIR=$PREFIX/share/vulkan/icd.d
MANIFEST=$DIR/nullmoth_nvk_icd.json
die() { echo "vulkan-for-apps: $*" >&2; exit 1; }
# the scratch-prefix override exists for the host test; a real install is root-only
need_root() { [ "$(id -u)" = 0 ] || [ -n "${NULLMOTH_VK_PREFIX:-}" ] || die "run with sudo"; }
case "${1:-}" in
  --install)
    need_root
    [ -f "$LIB" ] || die "the driver's NVK library is not at $LIB: install the driver first"
    # same trust rule as the plugin: a user-writable library would hand every Vulkan program to whoever can write it
    if [ -z "${NULLMOTH_VK_PREFIX:-}" ]; then
      [ "$(stat -f %u "$LIB")" = 0 ] && [ -z "$(find "$LIB" -perm -020 -o -perm -002 2>/dev/null)" ] || die "$LIB is not root-owned and write-protected"
    fi
    if [ "${2:-}" = "--with-loader" ]; then
      L=${3:?--with-loader needs the path of libvulkan.1.dylib}
      [ -f "$L" ] && lipo -archs "$L" 2>/dev/null | grep -qw x86_64 || die "$L is not an x86_64 library"
      mkdir -p "$PREFIX/lib"; cp "$L" "$PREFIX/lib/.libvulkan.new" && chmod 755 "$PREFIX/lib/.libvulkan.new" \
        && mv "$PREFIX/lib/.libvulkan.new" "$PREFIX/lib/libvulkan.1.dylib" || die "could not install the loader"
      echo "installed $PREFIX/lib/libvulkan.1.dylib"
    fi
    mkdir -p "$DIR"
    tmp=$(mktemp "$DIR/.nullmoth.XXXXXX")
    printf '{\n  "file_format_version": "1.0.0",\n  "ICD": {\n    "library_path": "%s",\n    "api_version": "1.3.0"\n  }\n}\n' "$LIB" > "$tmp"
    chmod 644 "$tmp"; mv "$tmp" "$MANIFEST"   # atomic: a loader never sees half a manifest
    echo "installed $MANIFEST" ;;
  --remove)
    need_root
    rm -f "$MANIFEST"; echo "removed $MANIFEST" ;;
  --status)
    if [ -f "$MANIFEST" ]; then echo "manifest: installed ($MANIFEST)"; else echo "manifest: not installed"; fi
    if [ -f "$LIB" ]; then echo "library: present ($LIB)"; else echo "library: missing ($LIB)"; fi ;;
  *) die "usage: vulkan-for-apps.sh --install | --remove | --status" ;;
esac
