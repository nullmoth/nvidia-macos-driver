#!/bin/bash
# fw_fetch.sh --dest DIR [--run FILE] [--installed DIR]
#
# Puts the GSP firmware for the NVIDIA 620.70.06 driver under DIR/nvidia/620.70.06/ without the package carrying it.
# The files are NVIDIA's, unmodified, taken from NVIDIA's own Linux driver package (the same pinned .run that
# dlss_fetch.sh reads), and every file is checked against a pinned SHA-256 before it is accepted.
#
# Order of sources, first that works:
#   1. --installed DIR (default /Library/NullMoth/nvfw): an earlier install whose four files all match the pins is reused,
#      so an update or a re-install with no network changes nothing.
#   2. --run FILE: a copy of NVIDIA-Linux-x86_64-620.70.06.run the user already downloaded (checked against its pin).
#   3. a download of that .run from download.nvidia.com (about 550 MB; only the four firmware files are kept).
#
# Exit 0 and the line "FW_OK <dir>" when DIR holds all four verified files. Any other outcome exits 1 after printing why
# and how to supply the file by hand; DIR is left empty. Nothing outside DIR (and a private temporary folder) is written,
# so a caller can run this before it changes anything.
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
HERE=$(cd "$(dirname "$0")" && pwd)

# ---- pins (keep URL_RUN / SHA_RUN equal to dlss_fetch.sh; tools/test_fw_fetch.py checks that) -----------------------
VER=620.70.06
URL_RUN="https://download.nvidia.com/XFree86/Linux-x86_64/$VER/NVIDIA-Linux-x86_64-$VER.run"
SHA_RUN=29118e3abd08c07559bcca3d9c76238d039490af0362a2e5bcfcbe7204afd4b1
# member in the .run -> SHA-256 of the file
FW_MEMBERS="firmware/gsp_tu10x.bin firmware/gsp_ga10x.bin firmware/ucodes_tu10x.bin firmware/ucodes_ga10x.bin"
fw_sha() {
  case "$1" in
    gsp_tu10x.bin)    echo 92af243e48c22325568ce24ef2edf5fa257b9bacc10a18fd38342cddeaf83fd8;;
    gsp_ga10x.bin)    echo 26a32926507e19e8c92911ed6a9b7981c9699c7a10bb78cc76b4f4abcbff84b6;;
    ucodes_tu10x.bin) echo db482009e834e26e901bfdf1f3428d1ab54b59b645ea9b987d0f6f14f4567a4f;;
    ucodes_ga10x.bin) echo b8f3d94d33cc284da4e0b758f069271818694b2ce8b50769d6a14b46df5d3cbf;;
    *) echo none;;
  esac
}
UNZSTD="$HERE/dlss/nm_unzstd"     # Zstandard reference decoder shipped in the package (build/build_dlss_cache.sh)
CURL_PROTO='=https'
INSTALLED=/Library/NullMoth/nvfw
# Tests may replace the pins and paths, but only when not running as root.
if [ "$(id -u)" != 0 ] && [ -n "${FW_TEST_PINS:-}" ]; then . "$FW_TEST_PINS" || exit 2; fi

DEST=""; RUN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dest) shift; DEST=${1:-};;
    --run) shift; RUN=${1:-};;
    --installed) shift; INSTALLED=${1:-};;
    *) echo "usage: fw_fetch.sh --dest DIR [--run FILE] [--installed DIR]" >&2; exit 2;;
  esac; shift
done
[ -n "$DEST" ] || { echo "usage: fw_fetch.sh --dest DIR [--run FILE] [--installed DIR]" >&2; exit 2; }
T=""
cleanup() { [ -z "$T" ] || rm -rf "$T"; }
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
fail() {
  echo "   STOP: $*" >&2
  echo "   The GPU firmware comes from NVIDIA's own driver package and is not part of this download." >&2
  echo "   To supply it by hand: download $URL_RUN" >&2
  echo "   (SHA-256 $SHA_RUN) and run this installer again with the file next to it as" >&2
  echo "   NVIDIA-Linux-x86_64-$VER.run, or pass it as --run FILE. Nothing on this Mac was changed." >&2
  rm -rf "$DEST/nvidia" 2>/dev/null
  exit 1
}
sha_of() { shasum -a 256 "$1" | awk '{print $1}'; }

OUT="$DEST/nvidia/$VER"
mkdir -p "$OUT" || fail "cannot create $OUT"

# 1. an earlier install whose files all match
have_all=1
for m in $FW_MEMBERS; do
  f=${m##*/}; src="$INSTALLED/nvidia/$VER/$f"
  [ -f "$src" ] && [ ! -L "$src" ] && [ "$(sha_of "$src")" = "$(fw_sha "$f")" ] || { have_all=0; break; }
done
if [ "$have_all" = 1 ]; then
  for m in $FW_MEMBERS; do f=${m##*/}; cp "$INSTALLED/nvidia/$VER/$f" "$OUT/$f" || fail "cannot copy $f"; done
  echo "   ok  the installed firmware already matches NVIDIA's $VER files"
  echo "FW_OK $DEST"; exit 0
fi

# 2./3. NVIDIA's package
T=$(mktemp -d /var/tmp/nullmoth-fw.XXXXXX) && [ -d "$T" ] && chmod 700 "$T" || fail "cannot create a private folder"
if [ -z "$RUN" ]; then
  # a copy the user left beside the installer
  for c in "$HERE/NVIDIA-Linux-x86_64-$VER.run" "$HERE/../NVIDIA-Linux-x86_64-$VER.run"; do [ -f "$c" ] && { RUN=$c; break; }; done
fi
if [ -n "$RUN" ]; then
  [ -f "$RUN" ] || fail "$RUN does not exist"
  cp "$RUN" "$T/driver.run" || fail "cannot read $RUN"
else
  echo "   downloading NVIDIA's $VER driver package (about 550 MB) to read the GPU firmware from it"
  curl -fL --proto "$CURL_PROTO" --tlsv1.2 --retry 3 --connect-timeout 20 -sS -o "$T/driver.run" "$URL_RUN" \
    || fail "the download from download.nvidia.com failed (no network?)"
fi
got=$(sha_of "$T/driver.run")
[ "$got" = "$SHA_RUN" ] || fail "the driver package does not match its pinned SHA-256 (got $got)"
echo "   ok  NVIDIA-Linux-x86_64-$VER.run matches its pin"
[ -x "$UNZSTD" ] || fail "this package lacks dlss/nm_unzstd"
# makeself archive: a shell header, then from line skip= a zstd-compressed tar (see dlss_fetch.sh for why not --extract-only)
SKIP=$(head -n 40 "$T/driver.run" | LC_ALL=C sed -n 's/^skip=\([0-9][0-9]*\)$/\1/p' | head -n 1)
case "$SKIP" in ""|*[!0-9]*) fail "the NVIDIA package header has no usable skip= line";; esac
[ "$SKIP" -gt 1 ] && [ "$SKIP" -lt 100000 ] || fail "the NVIDIA package header gives an unreasonable skip=$SKIP"
mkdir -p "$T/x" && chmod 700 "$T/x"
# shellcheck disable=SC2086
( set -o pipefail; tail -n +"$SKIP" "$T/driver.run" | "$UNZSTD" | /usr/bin/tar -x -C "$T/x" $FW_MEMBERS ) >"$T/extract.log" 2>&1 \
  || { tail -3 "$T/extract.log" >&2; fail "cannot unpack the firmware from the NVIDIA package"; }
for m in $FW_MEMBERS; do
  f=${m##*/}; src="$T/x/$m"
  [ -f "$src" ] && [ ! -L "$src" ] || fail "the NVIDIA package has no $m"
  [ "$(sha_of "$src")" = "$(fw_sha "$f")" ] || fail "$f does not match its pinned SHA-256"
  cp "$src" "$OUT/$f" || fail "cannot stage $f"
done
echo "   ok  four firmware files match their pins"
echo "FW_OK $DEST"
