#!/bin/bash
# stage_nmsmn.sh <NMSMN.kext> <ryzenadj> <sha256> <pkgroot>: puts the Ryzen power parts into a driver package root.
#   <pkgroot>/Library/NullMoth/kexts/NMSMN.kext      the SMU access kext (build: kexts/NMSMN/build.sh <out> 15.0)
#   <pkgroot>/Library/NullMoth/ryzenadj/ryzenadj     NullMoth's RyzenAdj build (tools/ryzenadj-macos/build.sh)
# Both are only carried: install.sh copies /Library/NullMoth/kexts as a whole and never loads NMSMN. The kext is inert until
# the boot argument nmsmn=1 is set, which only the Mac app's Ryzen power profile does, and the binary is installed by
# nullmoth-ryzen-power.sh only if its SHA-256 is pinned there. Refreshes the staged files' lines in SHA256SUMS when it exists;
# package/check_package.sh, the last step of a package build, refuses a package that lacks these parts. Refuses a kext that is not
# NullMoth's NMSMN, a binary that does not match <sha256>, and links anywhere.
set -eu
KEXT=${1:?usage: stage_nmsmn.sh <NMSMN.kext> <ryzenadj> <sha256> <pkgroot>}
BIN=${2:?}; SHA=${3:?}; ROOT=${4:?}
die() { echo "stage_nmsmn: $*" >&2; exit 1; }
[ -d "$KEXT" ] && [ ! -L "$KEXT" ] || die "$KEXT is not a kext folder"
[ -f "$BIN" ] && [ ! -L "$BIN" ] || die "$BIN is not a regular file"
[ -z "$(find "$KEXT" -type l)" ] || die "the kext holds a link"
case "$SHA" in *[!0-9a-f]*|'') die "the SHA-256 must be 64 lowercase hex digits";; esac
[ ${#SHA} -eq 64 ] || die "the SHA-256 must be 64 lowercase hex digits"
[ "$(plutil -extract CFBundleIdentifier raw -o - "$KEXT/Contents/Info.plist" 2>/dev/null)" = com.nullmoth.NMSMN ] || die "$KEXT is not the NullMoth NMSMN kext"
[ -f "$KEXT/Contents/MacOS/NMSMN" ] || die "the kext has no executable"
[ "$(shasum -a 256 "$BIN" | cut -d' ' -f1)" = "$SHA" ] || die "$BIN does not match $SHA"
mkdir -p "$ROOT/Library/NullMoth/kexts" "$ROOT/Library/NullMoth/ryzenadj"
rm -rf "$ROOT/Library/NullMoth/kexts/NMSMN.kext"
ditto "$KEXT" "$ROOT/Library/NullMoth/kexts/NMSMN.kext"
cp "$BIN" "$ROOT/Library/NullMoth/ryzenadj/ryzenadj"
chmod 755 "$ROOT/Library/NullMoth/ryzenadj/ryzenadj"
[ "$(shasum -a 256 "$ROOT/Library/NullMoth/ryzenadj/ryzenadj" | cut -d' ' -f1)" = "$SHA" ] || die "the staged copy does not match"
# SHA256SUMS: replace every line of the staged files (install.sh and the Mac app both verify against it)
if [ -f "$ROOT/SHA256SUMS" ]; then
  grep -vF -e "  ./Library/NullMoth/kexts/NMSMN.kext/" -e "  Library/NullMoth/kexts/NMSMN.kext/" \
       -e "  ./Library/NullMoth/ryzenadj/" -e "  Library/NullMoth/ryzenadj/" "$ROOT/SHA256SUMS" > "$ROOT/SHA256SUMS.new" || true
  (cd "$ROOT" && find Library/NullMoth/kexts/NMSMN.kext Library/NullMoth/ryzenadj -type f | LC_ALL=C sort | while IFS= read -r f; do
     printf '%s  ./%s\n' "$(shasum -a 256 "$f" | cut -d' ' -f1)" "$f"; done) >> "$ROOT/SHA256SUMS.new"
  mv -f "$ROOT/SHA256SUMS.new" "$ROOT/SHA256SUMS"
fi
echo "staged NMSMN.kext and ryzenadj ($SHA)"
