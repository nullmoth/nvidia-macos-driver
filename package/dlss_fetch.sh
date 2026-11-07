#!/bin/bash
# dlss_fetch.sh [--yes] [--force] [--sm N]
#
# Optional step: DLSS 4 / 4.5 support. The driver package carries no NVIDIA code for it. This script downloads
# NVIDIA's own files from NVIDIA's public sources onto this Mac, refuses anything whose SHA-256 differs from the pin
# below, and compiles the DLSS GPU kernels for this card with NVIDIA's nvJitLink (dlss/dlss_cache.c).
#
# (The frame-generation runtime is fetched alongside the other two.)
# Result, all under /Library/NullMoth/dlss (root-owned, not writable by others):
#   ngx/        NGX core               libnvidia-ngx.so.620.70.06
#   snippets/   DLSS runtimes          libnvidia-ngx-dlss.so.310.9.16, libnvidia-ngx-dlssd.so.310.9.16,
#               libnvidia-ngx-dlssg.so.310.9.16
#   kernels/    compiled GPU kernels   <BLAKE3 of the PTX object>.sm<N>.cubin
#   manifest.txt  versions, hashes and the SM they were built for
# Running it again with nothing changed does nothing. uninstall.sh removes the whole folder.
#
# The files are NVIDIA's and stay under NVIDIA's licences (DLSS SDK licence, driver licence, CUDA EULA).
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
HERE=$(cd "$(dirname "$0")" && pwd)

# ---- pins: every download is checked against these before it is opened -----------------------------------------
GH_RAW="https://raw.githubusercontent.com/NVIDIA/DLSS/main/lib/Linux_x86_64/rel"
URL_DLSS="$GH_RAW/libnvidia-ngx-dlss.so.310.9.16"
SHA_DLSS=30982c937afed85c96b8d313323de9a650a2eec6de1bb0f714b21750f4ce98f6
URL_DLSSD="$GH_RAW/libnvidia-ngx-dlssd.so.310.9.16"
SHA_DLSSD=a5d59de18841f15779039dda567a15dd723ba737915f489271fdb5f109fbfb96
URL_DLSSG="$GH_RAW/libnvidia-ngx-dlssg.so.310.9.16"
SHA_DLSSG=574839cae01c835de4d5845c36ca9cf75361f8160bd52397142c36a24e29e6f7
# NGX core: inside NVIDIA's Linux 620.70.06 driver package (about 550 MB; only one file is kept)
URL_RUN="https://download.nvidia.com/XFree86/Linux-x86_64/620.70.06/NVIDIA-Linux-x86_64-620.70.06.run"
SHA_RUN=29118e3abd08c07559bcca3d9c76238d039490af0362a2e5bcfcbe7204afd4b1
NGX_MEMBER=libnvidia-ngx.so.620.70.06
SHA_NGX=68ac7184bfc27f6b6fef1080a2054ae9919ef8c825421b4722d517c789e88942
# nvJitLink 12.9.86: the PyPI wheel (manylinux x86_64), then the one library inside it
URL_WHEEL="https://files.pythonhosted.org/packages/46/0c/c75bbfb967457a0b7670b8ad267bfc4fffdf341c074e0a80db06c24ccfd4/nvidia_nvjitlink_cu12-12.9.86-py3-none-manylinux2010_x86_64.manylinux_2_12_x86_64.whl"
SHA_WHEEL=e3f1171dbdc83c5932a45f0f4c99180a70de9bd2718c1ab77d14104f6d7147f9
JITLINK_MEMBER=nvidia/nvjitlink/lib/libnvJitLink.so.12
SHA_JITLINK=02d3acb5fe598dd20f0fca3cc03734ad164037a22747a01900561a42d0b8448f
DLSS_ROOT=/Library/NullMoth/dlss
CURL_PROTO='=https'
DLSS_TOOLS="$HERE/dlss"          # dlss_cache, nm_unzstd and assertfail_trap.ptx, built from dlss/ (build/build_dlss_cache.sh)
DEVICE_IDS=""                    # empty = read them from ioreg
# Tests may replace the pins and paths, but only when not running as root: a root run always uses the values above.
if [ "$(id -u)" != 0 ] && [ -n "${DLSS_TEST_PINS:-}" ]; then . "$DLSS_TEST_PINS" || exit 2; fi

YES=0; FORCE=0; SM=""
while [ $# -gt 0 ]; do
  case "$1" in
    --yes) YES=1;; --force) FORCE=1;;
    --sm) shift; SM=${1:-};;
    *) echo "usage: dlss_fetch.sh [--yes] [--force] [--sm N]" >&2; exit 2;;
  esac; shift
done
[ "${NULLMOTH_DLSS:-0}" != 1 ] || YES=1
T=""; NEW=""
die() { echo "   STOP: $*" >&2; exit 1; }
cleanup() { [ -z "$T" ] || rm -rf "$T"; [ -z "$NEW" ] || rm -rf "$NEW"; }
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
ok() { echo "   ok  $*"; }

[ "$(id -u)" = 0 ] || [ -n "${DLSS_TEST_PINS:-}" ] || die "run with sudo"

# ---- which SM does this card need? ------------------------------------------------------------------------------
# NVIDIA PCI device ids by family: Turing and the GTX 16 series 1E00-1FFF and 2100-21FF (sm_75), Ampere 2200-25FF
# (sm_86), Ada 2600-28FF (sm_89), Blackwell desktop and laptop 2B00-2DFF and 2F00-2FFF (sm_120). Anything else
# (data-centre parts, 2Exx) is not guessed: pass --sm.
sm_for_id() {
  local d=$((16#$1))
  if   [ $d -ge $((16#1E00)) ] && [ $d -le $((16#1FFF)) ]; then echo 75
  elif [ $d -ge $((16#2100)) ] && [ $d -le $((16#21FF)) ]; then echo 75
  elif [ $d -ge $((16#2200)) ] && [ $d -le $((16#22FF)) ]; then echo 86
  elif [ $d -ge $((16#2400)) ] && [ $d -le $((16#25FF)) ]; then echo 86
  elif [ $d -ge $((16#2600)) ] && [ $d -le $((16#28FF)) ]; then echo 89
  elif [ $d -ge $((16#2B00)) ] && [ $d -le $((16#2DFF)) ]; then echo 120
  elif [ $d -ge $((16#2F00)) ] && [ $d -le $((16#2FFF)) ]; then echo 120
  fi
}
if [ -z "$SM" ]; then
  if [ -z "$DEVICE_IDS" ]; then
    # Display-class functions only (PCI class 03xxxx: VGA 0300, 3D 0302). Every NVIDIA card also exposes an HDMI
    # audio function (class 0403) whose id sits in another family's range: an RTX 5060 (2D05) carries audio 22EB,
    # which read as Ampere and built sm_86 kernels for a Blackwell card.
    DEVICE_IDS=$(ioreg -r -c IOPCIDevice -d 1 | awk '/"vendor-id" = <de100000>/{nv=1}
      /"class-code" = </{ if (match($0, /<[0-9a-f]+>/)) c=substr($0, RSTART+1, 8) }
      /"device-id" = </{ if (match($0, /<[0-9a-f]+>/)) d=substr($0, RSTART+1, 4) }
      /^\+-o|^ *\}/{ if (nv && d != "" && substr(c,5,2) == "03") print toupper(substr(d,3,2) substr(d,1,2)); nv=0; d=""; c="" }' | sort -u)
  fi
  for i in $DEVICE_IDS; do
    case $i in *[!0-9A-F]*|"") continue;; esac
    s=$(sm_for_id "$i"); [ -z "$s" ] || { SM=$s; break; }
  done
  [ -n "$SM" ] || die "cannot tell which GPU architecture this card is (device ${DEVICE_IDS:-none}); pass --sm 75, 86, 89 or 120"
fi
case "$SM" in 75|80|86|87|89|90|100|120) ;; *) die "unsupported SM '$SM'";; esac

# ---- already done? ---------------------------------------------------------------------------------------------
WANT="dlss=$SHA_DLSS dlssd=$SHA_DLSSD dlssg=$SHA_DLSSG ngx=$SHA_NGX jitlink=$SHA_JITLINK sm=$SM"
have_manifest() { [ -f "$DLSS_ROOT/manifest.txt" ] && [ "$(sed -n 's/^pins: //p' "$DLSS_ROOT/manifest.txt")" = "$WANT" ]; }
if [ "$FORCE" = 0 ] && have_manifest && [ -f "$DLSS_ROOT/ngx/$NGX_MEMBER" ] && [ -d "$DLSS_ROOT/kernels" ]; then
  echo "DLSS files are already installed for sm_$SM (use --force to rebuild)."; exit 0
fi

# ---- opt in ----------------------------------------------------------------------------------------------------
if [ "$YES" != 1 ]; then
  [ -t 0 ] || { echo "DLSS 4 / 4.5 is optional and was not requested (run: sudo ./dlss_fetch.sh --yes)."; exit 0; }
  echo "DLSS 4 / 4.5 support downloads about 750 MB of NVIDIA files (DLSS SDK 310.9.16, NGX 620.70.06, nvJitLink 12.9.86)"
  echo "from NVIDIA's public sites and builds GPU kernels for this card (sm_$SM). The files stay under NVIDIA's licences."
  printf 'Download and install them? [y/N] '; read -r a
  case "$a" in y|Y|yes|YES) ;; *) echo "Skipped."; exit 0;; esac
fi

# ---- preconditions ---------------------------------------------------------------------------------------------
[ -x "$DLSS_TOOLS/dlss_cache" ] && [ -x "$DLSS_TOOLS/nm_unzstd" ] && [ -f "$DLSS_TOOLS/assertfail_trap.ptx" ] || die "this package lacks dlss/dlss_cache, dlss/nm_unzstd or dlss/assertfail_trap.ptx"
# As root this runs dlss_cache. A tool in a folder the logged-in user can write could be swapped after the package
# check, so as root every path it runs from must be root-owned and writable by root only (install.sh runs it from its
# verified private copy; a manual run needs the same).
if [ "$(id -u)" = 0 ]; then
  for p in "$HERE" "$DLSS_TOOLS" "$DLSS_TOOLS/dlss_cache" "$DLSS_TOOLS/nm_unzstd" "$DLSS_TOOLS/assertfail_trap.ptx" "$HERE/dlss_fetch.sh"; do
    [ ! -L "$p" ] && [ -z "$(find "$p" -maxdepth 0 \( ! -user root -o -perm -g+w -o -perm -o+w \) -print -quit)" ] || \
      die "$p is not root-owned and root-writable only; run this through install.sh (it checks and copies the package first)"
  done
fi
PARENT=$(dirname "$DLSS_ROOT")
[ -d "$PARENT" ] && [ ! -L "$PARENT" ] || die "$PARENT is missing or a link"
if [ -e "$DLSS_ROOT" ] || [ -L "$DLSS_ROOT" ]; then
  [ -d "$DLSS_ROOT" ] && [ ! -L "$DLSS_ROOT" ] || die "$DLSS_ROOT is not a plain folder"
  if [ "$(id -u)" = 0 ] && [ -n "$(find "$DLSS_ROOT" -maxdepth 0 \( ! -user root -o -perm -g+w -o -perm -o+w \) -print -quit)" ]; then
    die "$DLSS_ROOT is writable by someone other than root; remove it and run again"
  fi
fi
free_kb=$(df -k /var/tmp | awk 'NR==2{print $4}')
[ "${free_kb:-0}" -gt 2500000 ] || die "less than 2.5 GB free on /var/tmp"

T=$(mktemp -d /var/tmp/nullmoth-dlss.XXXXXX) && [ -d "$T" ] || die "create a private work folder"
chmod 700 "$T"

# fetch <name> <url> <sha256>: download, then refuse unless the hash matches
fetch() {
  echo "   downloading $1"
  curl -fL --proto "$CURL_PROTO" --tlsv1.2 --retry 3 --connect-timeout 20 -sS -o "$T/$1" "$2" || die "download of $1 failed"
  local got; got=$(shasum -a 256 "$T/$1" | awk '{print $1}')
  [ "$got" = "$3" ] || die "$1 does not match its pinned SHA-256 (got $got, want $3); nothing was installed"
  ok "$1 matches its pin"
}
member_check() { # member_check <file> <sha256>
  local got; got=$(shasum -a 256 "$1" | awk '{print $1}')
  [ "$got" = "$2" ] || die "$(basename "$1") does not match its pinned SHA-256 (got $got, want $2); nothing was installed"
}

fetch dlss.so "$URL_DLSS" "$SHA_DLSS"
fetch dlssd.so "$URL_DLSSD" "$SHA_DLSSD"
fetch dlssg.so "$URL_DLSSG" "$SHA_DLSSG"
fetch driver.run "$URL_RUN" "$SHA_RUN"
fetch nvjitlink.whl "$URL_WHEEL" "$SHA_WHEEL"

# the .run is NVIDIA's installer script; it is only unpacked after its hash matched
mkdir -p "$T/run" "$T/wheel" && chmod 700 "$T/run" "$T/wheel"
# The .run is a makeself archive: a shell header, then (from line `skip=`) a zstd-compressed tar. Its own --extract-only
# runs a bundled Linux zstd that a Mac cannot execute (and still exits 0 with nothing extracted), so the payload is read
# here: tail -> nm_unzstd (the reference decoder, shipped in the package) -> tar, taking only the one member we pin.
SKIP=$(head -n 40 "$T/driver.run" | LC_ALL=C sed -n 's/^skip=\([0-9][0-9]*\)$/\1/p' | head -n 1)
case "$SKIP" in ""|*[!0-9]*) die "the NVIDIA package header has no usable skip= line";; esac
[ "$SKIP" -gt 1 ] && [ "$SKIP" -lt 100000 ] || die "the NVIDIA package header gives an unreasonable skip=$SKIP"
mkdir -p "$T/run/x" && chmod 700 "$T/run/x"
( set -o pipefail; tail -n +"$SKIP" "$T/driver.run" | "$DLSS_TOOLS/nm_unzstd" | /usr/bin/tar -x -C "$T/run/x" "$NGX_MEMBER" ) >"$T/extract.log" 2>&1 \
  || die "unpacking the NVIDIA package failed (see $T/extract.log)"
[ -f "$T/run/x/$NGX_MEMBER" ] && [ ! -L "$T/run/x/$NGX_MEMBER" ] || die "the NVIDIA package has no $NGX_MEMBER"
member_check "$T/run/x/$NGX_MEMBER" "$SHA_NGX"
ok "$NGX_MEMBER matches its pin"
unzip -q -o "$T/nvjitlink.whl" "$JITLINK_MEMBER" -d "$T/wheel" || die "unpacking the nvJitLink wheel failed"
[ -f "$T/wheel/$JITLINK_MEMBER" ] && [ ! -L "$T/wheel/$JITLINK_MEMBER" ] || die "the wheel has no $JITLINK_MEMBER"
member_check "$T/wheel/$JITLINK_MEMBER" "$SHA_JITLINK"
ok "libnvJitLink.so.12 matches its pin"

# ---- build into a staging folder next to the destination, then swap ------------------------------------------
mkdir -p -m 755 "$DLSS_ROOT" || die "create $DLSS_ROOT"
NEW=$(mktemp -d "$DLSS_ROOT/.new.XXXXXX") && [ -d "$NEW" ] || die "create a staging folder in $DLSS_ROOT"
mkdir -m 755 "$NEW/ngx" "$NEW/snippets" "$NEW/kernels" || die "create staging folders"
# kernels already built for another card stay: their names carry the SM
[ ! -d "$DLSS_ROOT/kernels" ] || cp -p "$DLSS_ROOT/kernels/"*.cubin "$NEW/kernels/" 2>/dev/null
cp "$T/run/x/$NGX_MEMBER" "$NEW/ngx/$NGX_MEMBER" || die "stage the NGX core"
cp "$T/dlss.so" "$NEW/snippets/libnvidia-ngx-dlss.so.310.9.16" && cp "$T/dlssd.so" "$NEW/snippets/libnvidia-ngx-dlssd.so.310.9.16" \
  && cp "$T/dlssg.so" "$NEW/snippets/libnvidia-ngx-dlssg.so.310.9.16" || die "stage the DLSS runtimes"
echo "   building GPU kernels for sm_$SM (a few minutes)"
"$DLSS_TOOLS/dlss_cache" "$T/wheel/$JITLINK_MEMBER" "$SM" "$NEW/kernels" "$DLSS_TOOLS/assertfail_trap.ptx" \
  "$T/dlss.so" "$T/dlssd.so" "$T/dlssg.so" >"$T/cache.log" 2>&1
rc=$?
tail -n 5 "$T/cache.log"
[ "$rc" = 0 ] || die "the kernel build failed (exit $rc); nothing was installed"
n=$(find "$NEW/kernels" -name '*.cubin' | wc -l | tr -d ' ')
{
  echo "dlss_fetch 1"
  echo "dlss: 310.9.16"; echo "ngx: 620.70.06"; echo "nvjitlink: 12.9.86"
  echo "sm: $SM"; echo "kernels: $n"
  echo "pins: $WANT"
  echo "built: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$NEW/manifest.txt" || die "write the manifest"
if [ "$(id -u)" = 0 ]; then chown -R root:wheel "$NEW" || die "ownership of staged files"; fi
chmod -R go-w,a+rX "$NEW" || die "permissions of staged files"
for d in ngx snippets kernels; do
  rm -rf "$DLSS_ROOT/$d.old"
  [ ! -e "$DLSS_ROOT/$d" ] || mv "$DLSS_ROOT/$d" "$DLSS_ROOT/$d.old" || die "replace $d"
  mv "$NEW/$d" "$DLSS_ROOT/$d" || die "install $d"
  rm -rf "$DLSS_ROOT/$d.old"
done
mv -f "$NEW/manifest.txt" "$DLSS_ROOT/manifest.txt" || die "install the manifest"
ok "DLSS 4 / 4.5 files installed in $DLSS_ROOT ($n compiled kernels for sm_$SM)"
exit 0
