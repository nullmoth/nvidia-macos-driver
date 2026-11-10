#!/bin/bash
# build_xlate_release.sh <git-rev> <out-dir>: the release shader translator (libnvmtl_translate.dylib, x86_64) from
# translator/ at <git-rev>, cross-built on the build Mac (ssh host $NULLMOTH_BUILD_HOST) and copied back to <out-dir>.
# Source paths are remapped so the dylib names no build machine path; the build stops if one is left.
set -euo pipefail
REV=${1:?git revision}; OUT=${2:?output directory}
# The build Mac and its build folder come from the environment, so the public tree names no machine of ours.
H=${NULLMOTH_BUILD_HOST:?set NULLMOTH_BUILD_HOST to the build Mac ssh host}
BASE=${NULLMOTH_BUILD_ROOT:?set NULLMOTH_BUILD_ROOT to the build folder on that Mac}
HERE=$(cd "$(dirname "$0")/.." && pwd)
R=$BASE/build/xlate-release
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
git -C "$HERE" archive "$REV" translator | tar x -C "$TMP"
# The target directory is emptied before every build. git archive stamps each file with the commit time, and cargo
# rebuilds only when a source is newer than its last build output, so building a commit made before the previous
# build reused the previous build's objects and returned that dylib under the new revision.
# R stays fixed: cargo hashes the source path into symbol names, so the same commit built under another path is a
# different (equivalent) binary with a different UUID.
ssh "$H" "rm -rf $R/src $R/out $R/target && mkdir -p $R/src $R/out $R/target"
rsync -a "$TMP/translator/" "$H:$R/src/"
# The Rust toolchain is named, never found on PATH: with the pinned folder gone the build once fell through to another
# compiler that had no x86_64 target and failed with "can't find crate for core". NULLMOTH_RUST_BIN names the folder on
# the build Mac that holds cargo and rustc themselves (not rustup's proxies); NULLMOTH_RUST_VERSION the version expected
# there (the shipped translators are built with 1.98.1; another version builds the same source into different bytes).
RV=${NULLMOTH_RUST_VERSION:-1.98.1}
RB=${NULLMOTH_RUST_BIN:-\$HOME/nvmtl-build/rustup-home/toolchains/$RV-aarch64-apple-darwin/bin}
ssh "$H" "set -e
  TB=$RB
  [ -x \$TB/cargo ] && [ -x \$TB/rustc ] || { echo \"STOP: no cargo and rustc in \$TB (set NULLMOTH_RUST_BIN)\"; exit 3; }
  got=\$(\$TB/rustc --version | awk '{print \$2}')
  [ \"\$got\" = $RV ] || { echo \"STOP: rustc \$got in \$TB, not $RV (set NULLMOTH_RUST_VERSION to build with it knowingly)\"; exit 3; }
  [ -d \"\$(\$TB/rustc --print target-libdir --target x86_64-apple-darwin)\" ] || { echo \"STOP: the toolchain in \$TB has no x86_64-apple-darwin target\"; exit 3; }
  export PATH=\$TB:/usr/bin:/bin:/usr/sbin:/sbin CARGO_HOME=\$HOME/nvmtl-build/cargo-home
  export RUSTFLAGS=\"--remap-path-prefix=$R/src=/src --remap-path-prefix=\$CARGO_HOME=/cargo --remap-path-prefix=\$HOME=/build\"
  cd $R/src && cargo build --release --target x86_64-apple-darwin --manifest-path wrapper/Cargo.toml --target-dir $R/target > $R/out/cargo.log 2>&1 \
    || { grep -A6 '^error' $R/out/cargo.log | head -30; exit 4; }
  D=$R/out/libnvmtl_translate.dylib
  cp $R/target/x86_64-apple-darwin/release/libnvmtl_translate.dylib \$D
  strip -S \$D
  install_name_tool -id @rpath/libnvmtl_translate.dylib \$D
  codesign -f -s - \$D"
mkdir -p "$OUT"
scp -q "$H:$R/out/libnvmtl_translate.dylib" "$H:$R/out/cargo.log" "$OUT/"
D="$OUT/libnvmtl_translate.dylib"
bash "$HERE/build/warn_ceiling.sh" translator "$(grep -c '^warning' "$OUT/cargo.log" || true)" "$OUT/cargo.log" || exit 4
left=$(strings -a "$D" | grep -cE '/Users/|/Volumes/' || true)
[ "$left" = 0 ] || { echo "STOP: $left build machine path(s) left in the dylib"; strings -a "$D" | grep -E '/Users/|/Volumes/' | sort -u | head -5; exit 5; }
echo "libnvmtl_translate $(git -C "$HERE" rev-parse --short "$REV") sha $(shasum -a 256 "$D" | cut -c1-8) arch $(lipo -archs "$D") rustc $RV"
