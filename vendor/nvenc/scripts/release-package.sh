#!/bin/bash
# release-package.sh <tag> <out dir>: the release package of libnvencvt for a GitHub release of mickys/nvenc.
# Needs a clean tree with <tag> (annotated, vX.Y.Z) on HEAD: build.sh embeds `git describe` as the version in the encoder
# names, so the tag is made locally first and pushed only right before publishing. Builds in the guest (build.sh), takes the
# library back unsigned (installers sign it where they put it: the overlay's install.sh, or codesign -s - by hand) and writes
# <out>/nvenc-<X.Y.Z>-macos-x86_64.tar.gz (+ .sha256) with libnvencvt.dylib, LICENSE, README.md and SHA256SUMS.
#   NVENC_GUEST=<ssh host> as for build.sh (default: tahoe)
set -euo pipefail
cd "$(dirname "$0")/.."
TAG=${1:?usage: release-package.sh <tag> <out dir>}; OUT=${2:?usage: release-package.sh <tag> <out dir>}
G=${NVENC_GUEST:-tahoe}; V=${TAG#v}; N=nvenc-$V-macos-x86_64
[ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "the tree has changes: commit or stash them first"; exit 1; }
[ "$(git rev-parse "$TAG^{commit}" 2>/dev/null)" = "$(git rev-parse HEAD)" ] || { echo "$TAG is not on HEAD (git tag -a $TAG -m ... first)"; exit 1; }
[ "$(git describe --tags --always --dirty)" = "$TAG" ] || { echo "git describe says $(git describe --tags --always --dirty), not $TAG"; exit 1; }
./build.sh nvencvt
rm -rf "$OUT/$N"; mkdir -p "$OUT/$N"
scp -q "$G:nvenc-dev/libnvencvt.dylib" "$OUT/$N/libnvencvt.dylib"
grep -aq "$TAG" "$OUT/$N/libnvencvt.dylib" || { echo "the built library does not carry $TAG"; exit 1; }
cp LICENSE README.md "$OUT/$N/"
(cd "$OUT/$N" && sha256sum libnvencvt.dylib LICENSE README.md > SHA256SUMS)
tar -C "$OUT" --owner=0 --group=0 --numeric-owner -czf "$OUT/$N.tar.gz" "$N"
(cd "$OUT" && sha256sum "$N.tar.gz" > "$N.tar.gz.sha256")
echo "package $OUT/$N.tar.gz ($(stat -c %s "$OUT/$N.tar.gz") bytes, sha256 $(cut -c1-8 "$OUT/$N.tar.gz.sha256")); libnvencvt.dylib $(sha256sum "$OUT/$N/libnvencvt.dylib" | cut -c1-8)"
