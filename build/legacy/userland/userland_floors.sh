#!/bin/bash
# userland_floors.sh: test builds of the userland pieces other than the plugin and NVK at each macOS floor.
# Environment: AUDIT (audit folder holding tree/ = a copy of the driver tree), RUST_BIN (folder with cargo and rustc),
# CARGO_HOME, NVENC_HDR (headers folder of the codec library build), TOOLPATH (cmake, ninja). Nothing is installed.
set -u
A=${AUDIT:?}; T=$A/tree; U=$A/userland; mkdir -p "$U"; S=$U/summary.txt; : > "$S"
export PATH="${TOOLPATH:?}:$PATH"
say() { echo "$*" | tee -a "$S"; }
minos() { xcrun vtool -show-build "$1" 2>/dev/null | awk '/minos|version /{print $2; exit}'; }
FL=${FLOORS:-15.0 14.0 13.0 12.0 11.0 10.15 10.14 10.13 10.12 10.11}

# the Rust translator: the toolchain's own lowest target first, then one step below it
for F in ${XFLOORS:-10.12 10.11}; do
  O=$U/xlate-$F; mkdir -p "$O"
  ( export PATH="${RUST_BIN:?}:/usr/bin:/bin:/usr/sbin:/sbin" MACOSX_DEPLOYMENT_TARGET=$F
    cd "$T/translator" && cargo build --offline --release --target x86_64-apple-darwin --manifest-path wrapper/Cargo.toml --target-dir "$O/target" > "$O/cargo.log" 2>&1 ); rc=$?
  D=$O/target/x86_64-apple-darwin/release/libnvmtl_translate.dylib
  [ -f "$D" ] && cp "$D" "$O/libnvmtl_translate.dylib"
  say "translator floor $F rc=$rc dylib=$([ -f "$D" ] && echo yes || echo no) minos=$([ -f "$D" ] && minos "$D") warnings=$(grep -c '^warning' "$O/cargo.log") errors=$(grep -c '^error' "$O/cargo.log")"
  rm -rf "$O/target"
done

# the Vulkan loader, from the pinned release
L=$U/loader-src
if [ ! -d "$L/Vulkan-Loader" ]; then
  mkdir -p "$L"
  git clone -q --depth 1 --branch v1.4.367 https://github.com/KhronosGroup/Vulkan-Headers "$L/Vulkan-Headers"
  git clone -q --depth 1 --branch v1.4.367 https://github.com/KhronosGroup/Vulkan-Loader "$L/Vulkan-Loader"
fi
for F in $FL; do
  O=$U/loader-$F; rm -rf "$O"; mkdir -p "$O"
  ARCH="-DCMAKE_OSX_ARCHITECTURES=x86_64 -DCMAKE_OSX_DEPLOYMENT_TARGET=$F -DCMAKE_BUILD_TYPE=Release"
  ( cmake -S "$L/Vulkan-Headers" -B "$O/hb" -G Ninja $ARCH -DCMAKE_INSTALL_PREFIX="$O/headers" && cmake --install "$O/hb" &&
    cmake -S "$L/Vulkan-Loader" -B "$O/lb" -G Ninja $ARCH -DUPDATE_DEPS=OFF -DBUILD_TESTS=OFF -DVULKAN_HEADERS_INSTALL_DIR="$O/headers" -DCMAKE_INSTALL_PREFIX="$O/loader" &&
    cmake --build "$O/lb" -j6 && cmake --install "$O/lb" ) > "$O/build.log" 2>&1; rc=$?
  D=$(ls "$O"/loader/lib/libvulkan.1.*.*.dylib 2>/dev/null | head -1)
  [ -n "$D" ] && cp "$D" "$O/libvulkan.1.dylib"
  say "loader floor $F rc=$rc dylib=$([ -n "$D" ] && echo yes || echo no) minos=$([ -n "$D" ] && minos "$D") errors=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:' "$O/build.log") warnings=$(grep -c 'warning:' "$O/build.log")"
  rm -rf "$O/hb" "$O/lb" "$O/headers" "$O/loader"
done

# the video codec library, the bitcode lister and the version library
H=${NVENC_HDR:?}
for F in $FL; do
  O=$U/small-$F; rm -rf "$O"; mkdir -p "$O"
  ( cd "$T/vendor/nvenc" && xcrun clang -arch x86_64 -mmacosx-version-min=$F -O2 -Wall -fvisibility=hidden -dynamiclib -DNVENC_VERSION='"test"' \
      -o "$O/libnvencvt.dylib" vt/nvencvt.c vt/nvencvt_dec.c src/*.c -I src -I "$H/webkit-spi" -I "$H/ogkm/src/common/sdk/nvidia/inc" \
      -I "$H/ogkm/src/nvidia/arch/nvalloc/unix/include" -I "$H/ogd/classes/video" -Wunguarded-availability -ferror-limit=0 \
      -framework IOKit -framework CoreFoundation -framework CoreMedia -framework CoreVideo -framework VideoToolbox -framework Accelerate -framework IOSurface ) > "$O/nvencvt.err" 2>&1; r1=$?
  xcrun clang -arch x86_64 -mmacosx-version-min=$F -O2 -Wall -Wextra -Wunguarded-availability -o "$O/nvmtl-air-dis" "$T/plugin/air_dis.c" > "$O/airdis.err" 2>&1; r2=$?
  xcrun clang -arch x86_64 -mmacosx-version-min=$F -dynamiclib -fobjc-arc -O2 -Wall -Wunguarded-availability -I"$T/plugin" -o "$O/libnvmtl_osver.dylib" "$T/plugin/nvmtl_osver.m" \
      -framework Foundation -framework CoreFoundation > "$O/osver.err" 2>&1; r3=$?
  c() { echo "rc=$1 errors=$(grep -cE '^[^ ]+:[0-9]+:[0-9]+: (fatal )?error:' "$2") avail=$(grep -c 'is only available on macOS' "$2") other-warnings=$(grep -E '^[^ ]+:[0-9]+:[0-9]+: warning:' "$2" | grep -vc 'only available')"; }
  say "codecs floor $F $(c $r1 "$O/nvencvt.err") | air-dis $(c $r2 "$O/airdis.err") | osver $(c $r3 "$O/osver.err")"
done
say ALL-DONE
