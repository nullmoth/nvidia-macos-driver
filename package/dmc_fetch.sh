#!/bin/bash
# dmc_fetch.sh --dest DIR [--installed DIR]
#
# Puts Intel's display microcontroller firmware under DIR/i915/ without the package carrying it. The files are Intel's,
# unmodified, from the linux-firmware repository at one pinned commit, redistributable in binary form unmodified under
# Intel's licence (licenses/Intel-i915-firmware-LICENSE.txt); every file is checked against a pinned SHA-256 before it is kept.
# Used by the hybrid-laptop panel driver (docs/HYBRID-LAPTOPS.md) on display generations that need it; nothing reads these
# files unless that driver is switched on.
#
# Order of sources, first that works:
#   1. --installed DIR (default /Library/GPUBundles/nvmtl): an earlier install whose files all match the pins is reused.
#   2. a download of each file from git.kernel.org at the pinned commit (20 to 80 KB each).
#
# Exit 0 and the line "DMC_OK <dir>" when DIR holds every verified file. Any other outcome exits 1 after printing why;
# DIR is left without partial files.
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/dmc_pins.sh" || exit 2
INSTALLED=/Library/GPUBundles/nvmtl
CURL_PROTO='=https'
# Tests may replace the pins and the source, but only when not running as root.
if [ "$(id -u)" != 0 ] && [ -n "${DMC_TEST_PINS:-}" ]; then . "$DMC_TEST_PINS" || exit 2; fi

DEST=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dest) shift; DEST=${1:-};;
    --installed) shift; INSTALLED=${1:-};;
    *) echo "usage: dmc_fetch.sh --dest DIR [--installed DIR]" >&2; exit 2;;
  esac; shift
done
[ -n "$DEST" ] || { echo "usage: dmc_fetch.sh --dest DIR [--installed DIR]" >&2; exit 2; }
T=""
cleanup() { [ -z "$T" ] || rm -rf "$T"; }
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
sha_of() { /usr/bin/shasum -a 256 "$1" 2>/dev/null | cut -d' ' -f1; }
fail() { echo "   STOP: $*" >&2; echo "   The display firmware is optional; the hybrid-laptop path works without it on Intel generations 12 and 13." >&2; exit 1; }

all_ok_in() {   # all pinned files present and matching in $1
  local f
  for f in $FILES; do [ -f "$1/$f" ] && [ "$(sha_of "$1/$f")" = "$(dmc_sha "$f")" ] || return 1; done
  return 0
}

if all_ok_in "$INSTALLED/i915"; then
  mkdir -p "$DEST/i915" || fail "cannot create $DEST/i915"
  for f in $FILES; do cp -p "$INSTALLED/i915/$f" "$DEST/i915/$f" || fail "cannot copy $f"; done
  echo "DMC_OK $DEST/i915 (reused from $INSTALLED)"; exit 0
fi

T=$(mktemp -d "${TMPDIR:-/tmp}/dmcfetch.XXXXXX") || fail "no temporary folder"
for f in $FILES; do
  /usr/bin/curl --proto "$CURL_PROTO" --tlsv1.2 -fsSL -m 120 -o "$T/$f" "$BASE/$f?id=$COMMIT" || fail "could not download $f"
  [ "$(sha_of "$T/$f")" = "$(dmc_sha "$f")" ] || fail "$f does not match its pinned SHA-256; it was not kept"
done
mkdir -p "$DEST/i915" || fail "cannot create $DEST/i915"
for f in $FILES; do mv "$T/$f" "$DEST/i915/$f" || fail "cannot place $f"; done
echo "DMC_OK $DEST/i915"
