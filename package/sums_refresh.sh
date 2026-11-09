#!/bin/bash
# sums_refresh.sh <pkgroot> <path in the package>...: replace the SHA256SUMS lines of the named files with their current
# hashes (written as ./<path>, the form stage_nmsmn.sh writes). Staging scripts call it so a file they add is listed, which
# sums_complete.sh and install.sh require. Does nothing when the tree has no SHA256SUMS yet (finish_package.sh writes it).
set -euo pipefail
ROOT=${1:?pkgroot}; shift
[ -f "$ROOT/SHA256SUMS" ] || exit 0
tmp="$ROOT/SHA256SUMS.new"
cp "$ROOT/SHA256SUMS" "$tmp"
for f in "$@"; do
  [ -f "$ROOT/$f" ] && [ ! -L "$ROOT/$f" ] || { rm -f "$tmp"; echo "sums_refresh: $f is not a regular file in the package" >&2; exit 1; }
  # drop every line naming this path, in either spelling, then add the current one
  awk -v a="$f" -v b="./$f" '{ p = substr($0, 67) } !(p == a || p == b)' "$tmp" > "$tmp.2"
  mv -f "$tmp.2" "$tmp"
  printf '%s  ./%s\n' "$(shasum -a 256 "$ROOT/$f" | cut -d' ' -f1)" "$f" >> "$tmp"
done
mv -f "$tmp" "$ROOT/SHA256SUMS"
