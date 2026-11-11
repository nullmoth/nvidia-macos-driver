#!/bin/bash
# xlate_floor.sh <driver tree> <out dir>: the shader translator for OS X 10.11. Test build, nothing is installed.
# The Rust toolchain's own floor is 10.12, so the library is built for 10.12 with the one call 10.11 lacks defined
# inside it (translator/wrapper/legacy/floor_shim.c), and its recorded minimum system is then set to 10.11.
# Environment: RUST_BIN (folder with cargo and rustc), CARGO_HOME.
set -u
T=${1:?driver tree}; O=${2:?out dir}; mkdir -p "$O"; O=$(cd "$O" && pwd)
xcrun clang -arch x86_64 -mmacosx-version-min=10.11 -O2 -Wall -Wno-unguarded-availability -c -o "$O/floor_shim.o" "$T/translator/wrapper/legacy/floor_shim.c" || exit 1
( export PATH="${RUST_BIN:?}:/usr/bin:/bin:/usr/sbin:/sbin" MACOSX_DEPLOYMENT_TARGET=10.12 RUSTFLAGS="-C link-arg=$O/floor_shim.o"
  cd "$T/translator" && cargo build --offline --release --target x86_64-apple-darwin --manifest-path wrapper/Cargo.toml --target-dir "$O/target" > "$O/cargo.log" 2>&1 )
rc=$?
D=$O/target/x86_64-apple-darwin/release/libnvmtl_translate.dylib
[ -f "$D" ] || { echo "translator 10.11: build failed rc=$rc"; exit 1; }
cp "$D" "$O/libnvmtl_translate.dylib"; strip -S "$O/libnvmtl_translate.dylib"
SDKV=$(xcrun --show-sdk-version)
xcrun vtool -set-build-version macos 10.11 "$SDKV" -replace -output "$O/libnvmtl_translate.dylib" "$O/libnvmtl_translate.dylib" > /dev/null 2>&1 \
  || xcrun vtool -set-version-min macos 10.11 "$SDKV" -replace -output "$O/libnvmtl_translate.dylib" "$O/libnvmtl_translate.dylib"
install_name_tool -id @rpath/libnvmtl_translate.dylib "$O/libnvmtl_translate.dylib"; codesign -f -s - "$O/libnvmtl_translate.dylib" 2>/dev/null
rm -rf "$O/target"
echo "translator 10.11: rc=$rc minimum $(xcrun vtool -show-build "$O/libnvmtl_translate.dylib" | awk '/minos|version /{print $2; exit}') exports $(nm -gU "$O/libnvmtl_translate.dylib" | wc -l | tr -d ' ')"
