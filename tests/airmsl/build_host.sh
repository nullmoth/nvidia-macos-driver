#!/bin/bash
# build_host.sh <src-dir> <build-dir>: build the translator wrapper (libnvmtl_translate.dylib) for the build Mac's own
# architecture from <src-dir>/translator, for running the shader corpus there. Release shipping builds use
# build/build_xlate_release.sh instead.
set -euo pipefail
SRC=${1:?translator parent dir}; B=${2:?build dir}
TC=$HOME/nvmtl-build/rustup-home/toolchains/1.98.1-aarch64-apple-darwin
export PATH=$TC/bin:/opt/homebrew/bin:/usr/bin:/bin CARGO_HOME=$HOME/nvmtl-build/cargo-home RUSTUP_HOME=$HOME/nvmtl-build/rustup-home
mkdir -p "$B"
cargo build --release --manifest-path "$SRC/translator/wrapper/Cargo.toml" --target-dir "$B" 2>&1 | tail -3
ls -l "$B/release/libnvmtl_translate.dylib"
