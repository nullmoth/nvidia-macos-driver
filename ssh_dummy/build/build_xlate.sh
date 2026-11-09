#!/bin/bash
set -u
export PATH=$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin
N=$HOME/nvmtl-build; X=${X:-$N/bindless}; L=$N/build.lock
cd $X || exit 2
[ -z "$(git status --porcelain)" ] || { echo "STOP: translator tree has uncommitted edits:"; git status --short | head; exit 3; }
C=$(git rev-parse --short HEAD); O=$N/LIVE/out/xlate-$C; mkdir -p $O
until mkdir $L 2>/dev/null; do sleep 5; done; echo "build_xlate $C $$" > $L/owner; trap 'rm -rf "$L"' EXIT
export RUSTFLAGS="--remap-path-prefix=$X=/src --remap-path-prefix=$HOME/.cargo=/cargo --remap-path-prefix=$HOME=/build"
cargo build --release --manifest-path wrapper/Cargo.toml --target-dir target > $O/cargo.log 2>&1; rc=$?
grep -E "^error|warning: unused" $O/cargo.log | head -10
[ "$rc" = 0 ] || { echo "STOP: cargo build failed (rc $rc) — $O/cargo.log"; exit 4; }
cp target/release/libnvmtl_translate.dylib $O/; D=$O/libnvmtl_translate.dylib; strip -S $D   # debug-map stabs name every .o by absolute path
install_name_tool -id @rpath/libnvmtl_translate.dylib $D 2>/dev/null && codesign -f -s - $D 2>/dev/null
miss=0
for p in NVMTL_NO_BINDLESS_ALL NVMTL_NO_TIER2; do [ "$(strings -a $D | grep -c -- $p)" -ge 1 ] || { echo "  MISSING marker $p"; miss=$((miss+1)); }; done
gone=$(comm -23 $N/LIVE/xlate.exports <(nm -gU $D | awk '{print $3}' | sort) | wc -l | tr -d ' ')
[ "$gone" = 0 ] || { echo "  MISSING $gone export(s)"; miss=$((miss+1)); }
echo "$X $(git rev-parse HEAD)" > $O/TREE
echo "  tree: $X @ $C   identity: $(python3 $N/shipcheck.py $D | tail -1)"
echo "xlate-$C  sha $(shasum -a256 $D | cut -c1-8)  uuid $(otool -l $D | awk '/uuid/{print $2; exit}')  missing $miss"
[ $miss = 0 ] || { echo "STOP: do NOT stage"; exit 6; }
echo "$D"
