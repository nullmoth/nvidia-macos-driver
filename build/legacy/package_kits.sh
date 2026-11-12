#!/bin/bash
# package_kits.sh <version> : one installable archive per older release from the kits assemble_kits.sh gathered.
#   $B/kit/<release>  ->  $B/pkg/nullmoth-nvidia-legacy-<release>-<version>.tar.gz   (+ $B/pkg/legacy_kits.json for the builder)
# Each archive holds the kit, the installer (package/legacy), the firmware fetcher, and a Zstandard decoder built for
# 10.7 so the same file runs on every release. Reference data files a test package left in the kit are replaced or
# dropped here. Nothing is installed or run. $SRC = the driver source tree (default: the folder this script is in).
set -u
B=${B:?audit folder}; V=${1:?version}
SRC=${SRC:-$(cd "$(dirname "$0")/../.." && pwd)}
P=$B/pkg; rm -rf "$P"; mkdir -p "$P/tools"
die() { echo "STOP $*"; exit 1; }
# one decoder for all releases: 10.7 is the oldest release a kit is packaged for
xcrun clang -O2 -arch x86_64 -mmacosx-version-min=10.7 -DZSTD_DISABLE_ASM -o "$P/tools/nm_unzstd" "$SRC/dlss/nm_unzstd.c" \
  "$SRC"/dlss/zstd/common/*.c "$SRC"/dlss/zstd/decompress/*.c 2>"$P/tools/unzstd.log" || { cat "$P/tools/unzstd.log"; die "decoder build"; }
strip -S "$P/tools/nm_unzstd"
xcrun otool -l "$P/tools/nm_unzstd" | grep -q "version 10.7" || die "the decoder is not built for 10.7"
printf '{\n  "version": "%s",\n  "base": "https://github.com/nullmoth/nvidia-macos-driver/releases/download/v%s/",\n  "kits": {' "$V" "$V" > "$P/legacy_kits.json"
first=1
for rel in 10.7 10.8 10.9 10.10 10.11 10.12 10.13 10.14 10.15 11 12 13 14; do
  K=$B/kit/$rel; [ -d "$K/kexts/NVRM.kext" ] || die "no kit for $rel"
  N=nullmoth-nvidia-legacy-$rel-$V; D=$P/$N; mkdir -p "$D/dlss"
  cp -R "$K/kexts" "$D/" || die "copy kexts $rel"
  for d in opengl options held-back; do [ -d "$K/$d" ] && cp -R "$K/$d" "$D/"; done
  if [ -d "$K/userland/payload" ]; then
    mkdir -p "$D/userland" && cp -R "$K/userland/payload" "$D/userland/" || die "copy userland $rel"
    GB=$D/userland/payload/Library/GPUBundles
    rm -f "$GB/nvmtl-allow.txt"                       # a test package's allow list: never shipped
    /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $V" -c "Set :CFBundleVersion $V" "$GB/NVMTLDriver.bundle/Contents/Info.plist" || die "bundle version $rel"
  fi
  cp "$K/README.txt" "$D/README.txt"
  cp "$SRC/package/legacy/install-legacy.sh" "$SRC/package/legacy/uninstall-legacy.sh" "$SRC/package/fw_fetch.sh" "$D/" || die "copy scripts"
  cp "$P/tools/nm_unzstd" "$D/dlss/nm_unzstd"; cp "$SRC/dlss/zstd/LICENSE" "$D/dlss/nm_unzstd.LICENSE"
  cp "$SRC/LICENSE" "$D/LICENSE"
  printf '%s\n' "$rel" > "$D/RELEASE"; printf '%s\n' "$V" > "$D/VERSION"
  chmod 755 "$D"/*.sh "$D/dlss/nm_unzstd"
  # every program the kit carries is signed (ad hoc); the kexts keep the signatures of their build
  codesign -f -s - "$D/dlss/nm_unzstd" >/dev/null 2>&1 || die "sign decoder"
  find "$D" -name .DS_Store -delete
  ( cd "$D" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 shasum -a 256 > SHA256SUMS ) || die "sums $rel"
  ( cd "$D" && shasum -a 256 -c SHA256SUMS >/dev/null ) || die "sums do not verify $rel"
  # a plain ustar archive without extended attributes or resource forks: 10.7's tar must read it
  ( cd "$P" && COPYFILE_DISABLE=1 tar --format ustar --no-xattrs --no-mac-metadata -czf "$N.tar.gz" "$N" ) || die "archive $rel"
  s=$(shasum -a 256 "$P/$N.tar.gz" | cut -d' ' -f1)
  [ $first = 1 ] || printf ',' >> "$P/legacy_kits.json"; first=0
  printf '\n    "%s": {"name": "%s.tar.gz", "sha256": "%s"}' "$rel" "$N" "$s" >> "$P/legacy_kits.json"
  echo "$rel  $(du -h "$P/$N.tar.gz" | cut -f1)  $s  files=$(wc -l < "$D/SHA256SUMS" | tr -d ' ')"
  rm -rf "$D"
done
printf '\n  }\n}\n' >> "$P/legacy_kits.json"
python3 -c "import json,sys; j=json.load(open(sys.argv[1])); assert len(j['kits'])==13" "$P/legacy_kits.json" || die "the pin file is not valid"
( cd "$P" && shasum -a 256 *.tar.gz > SHA256SUMS )
echo "pins: $P/legacy_kits.json"
