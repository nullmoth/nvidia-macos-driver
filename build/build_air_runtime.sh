#!/bin/bash
# build_air_runtime.sh <LLVM 21.1.8 source dir> <out dir> <zstd include dir> <libzstd.1.dylib>
# The LLVM, cmake and third-party source archives belong to the same release; cmake/ and third-party/ must be
# siblings of the LLVM source directory. Produces <out>/air-runtime and <out>/LICENSE.txt; needs CMake and Ninja.
# Only the reader, IR writer and optimizer are needed here. The native GPU compiler remains the separate NVK/NAK build.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
SRC=$(cd "${1:?LLVM source dir}" && pwd)
OUT=${2:?out dir}; ZSTD_INCLUDE=${3:?zstd include dir}; ZSTD_LIB=${4:?libzstd.1.dylib}
CMAKE=${CMAKE:-cmake}; NINJA=${NINJA:-ninja}; JOBS=${JOBS:-8}
VERSION="$SRC/../cmake/Modules/LLVMVersion.cmake"
for pair in MAJOR:21 MINOR:1 PATCH:8; do
  grep -Eq "set\(LLVM_VERSION_${pair%:*} +${pair#*:}\)" "$VERSION" || { echo "STOP: LLVM 21.1.8 sources are required" >&2; exit 1; }
done
[ -f "$SRC/../third-party/siphash/include/siphash/SipHash.h" ] || { echo "STOP: matching third-party sources are missing" >&2; exit 1; }
[ -f "$ZSTD_INCLUDE/zstd.h" ] && [ -f "$ZSTD_LIB" ] || { echo "STOP: zstd headers and library are required" >&2; exit 1; }
mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
mkdir -p "$OUT/deps" "$OUT/bin" "$OUT/lib" "$OUT/air-runtime"
cp "$ZSTD_LIB" "$OUT/deps/libzstd.1.dylib"
chmod 755 "$OUT/deps/libzstd.1.dylib"
install_name_tool -id @loader_path/libzstd.1.dylib "$OUT/deps/libzstd.1.dylib"
codesign -f -s - "$OUT/deps/libzstd.1.dylib"
# LLVM's build-time table generator also links zstd and runs from bin/.
cp "$OUT/deps/libzstd.1.dylib" "$OUT/bin/"
cp "$OUT/deps/libzstd.1.dylib" "$OUT/lib/"

# RTLD_LOCAL does not isolate these weak C++ definitions from Apple's LLVM in the dyld shared cache. Restrict exports
# at link time so LLVM's internal calls bind to its own implementation, regardless of which library was loaded first.
# Editing the symbol table of an already linked dylib is insufficient: its dyld fixups and export trie also matter.
"$CMAKE" -G Ninja -S "$SRC" -B "$OUT" \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER="$(xcrun --find clang)" -DCMAKE_CXX_COMPILER="$(xcrun --find clang++)" \
  -DCMAKE_MAKE_PROGRAM="$(command -v "$NINJA")" -DCMAKE_OSX_ARCHITECTURES=x86_64 -DCMAKE_OSX_DEPLOYMENT_TARGET=15.0 \
  -DLLVM_BUILD_LLVM_DYLIB=ON -DLLVM_LINK_LLVM_DYLIB=OFF \
  '-DLLVM_DYLIB_COMPONENTS=core;irreader;bitreader;bitwriter;passes' \
  -DLLVM_DYLIB_EXPORTED_SYMBOL_FILE="$HERE/air_runtime.exports" \
  -DLLVM_TARGETS_TO_BUILD= -DLLVM_ENABLE_PROJECTS= -DLLVM_ENABLE_BINDINGS=OFF \
  -DLLVM_INCLUDE_TESTS=OFF -DLLVM_INCLUDE_BENCHMARKS=OFF -DLLVM_INCLUDE_EXAMPLES=OFF -DLLVM_BUILD_TOOLS=OFF \
  -DLLVM_ENABLE_ASSERTIONS=OFF -DLLVM_ENABLE_ZLIB=ON -DLLVM_ENABLE_ZSTD=ON \
  -Dzstd_INCLUDE_DIR="$ZSTD_INCLUDE" -Dzstd_LIBRARY="$OUT/deps/libzstd.1.dylib" \
  -DLLVM_ENABLE_LIBXML2=OFF -DLLVM_ENABLE_LIBEDIT=OFF
"$CMAKE" --build "$OUT" --target LLVM --parallel "$JOBS"
cp "$OUT/lib/libLLVM.dylib" "$OUT/air-runtime/libLLVM.dylib"
cp "$OUT/deps/libzstd.1.dylib" "$OUT/air-runtime/"
cp "$SRC/LICENSE.TXT" "$OUT/LICENSE.txt"
install_name_tool -id @loader_path/libLLVM.dylib "$OUT/air-runtime/libLLVM.dylib"
codesign -f -s - "$OUT/air-runtime/libLLVM.dylib"
python3 "$HERE/check_air_runtime.py" "$OUT/air-runtime/libLLVM.dylib"
echo "built private AIR runtime in $OUT/air-runtime"
