#!/bin/bash
# build.sh <RyzenAdj checkout> <out dir>: builds RyzenAdj for macOS (x86_64, macOS 15 and later) from an unmodified upstream
# checkout plus osdep_macos.c. The checkout must be at the pinned tag and commit below; anything else is refused.
# Only Apple's compiler and SDK are used (no CMake, no libpci). Prints the SHA-256 of the result for ryzenadj.sha256.
set -eu
TAG=v0.19.0
COMMIT=ea71739b4a3e1a0a624dfd1c9c268a31cb2a4182
HERE=$(cd "$(dirname "$0")" && pwd)
SRC=${1:?usage: build.sh <RyzenAdj checkout> <out dir>}
OUT=${2:?usage: build.sh <RyzenAdj checkout> <out dir>}
[ "$(git -C "$SRC" rev-parse HEAD)" = "$COMMIT" ] || { echo "STOP: $SRC is not at $TAG ($COMMIT)"; exit 1; }
[ -z "$(git -C "$SRC" status --porcelain)" ] || { echo "STOP: $SRC has changes; the build uses the upstream tree as tagged"; exit 1; }
mkdir -p "$OUT"
# a reproducible binary: no build paths, no timestamps in the Mach-O (the linker's own UUID is dropped)
xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -O2 -std=gnu2x -D_LIBRYZENADJ_INTERNAL -DNDEBUG \
  -ffile-prefix-map="$SRC"=ryzenadj -ffile-prefix-map="$HERE"=nullmoth -g0 -Wl,-no_uuid \
  -Wno-deprecated-declarations \
  -I"$SRC/lib" "$HERE/osdep_macos.c" "$SRC/lib/nb_smu_ops.c" "$SRC/lib/api.c" "$SRC/lib/cpuid.c" "$SRC/argparse.c" "$SRC/main.c" \
  -framework IOKit -framework CoreFoundation -o "$OUT/ryzenadj"
strip -x "$OUT/ryzenadj"
# the entitlement NMSMN requires of its caller, in an ad-hoc signature (deterministic: no timestamp, no identity)
cat > "$OUT/ryzenadj.entitlements" <<'ENT'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.nullmoth.ryzenadj</key><true/></dict></plist>
ENT
codesign --force -s - --entitlements "$OUT/ryzenadj.entitlements" -i com.nullmoth.ryzenadj "$OUT/ryzenadj"
echo "RyzenAdj $TAG $COMMIT"
shasum -a 256 "$OUT/ryzenadj"
