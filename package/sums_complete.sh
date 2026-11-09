#!/bin/bash
# sums_complete.sh <pkgroot>: every regular file in a package tree must have a line in its SHA256SUMS.
# `shasum -c SHA256SUMS` only checks the files the list names; a file the list leaves out passes it unread, and install.sh
# copies whole folders (Library/NullMoth/kexts, the bundles) as root. install.sh runs this after `shasum -c` on its private
# copy, and check_package.sh runs it at build time. Links are not listed (install.sh refuses any that leave the package);
# their targets are regular files and are listed. Exit 1 with the unlisted paths, 0 when the list is complete.
set -uo pipefail
ROOT=${1:?pkgroot}
[ -f "$ROOT/SHA256SUMS" ] || { echo "STOP: $ROOT has no SHA256SUMS"; exit 1; }
# A path holding a newline cannot be matched against a line-based list, so it is refused rather than skipped.
if [ -n "$(cd "$ROOT" && find . -name $'*\n*' -print -quit)" ]; then echo "STOP: a package path holds a newline"; exit 1; fi
listed=$(sed -E 's/^[0-9a-f]{64} [ *](\.\/)?//' "$ROOT/SHA256SUMS" | LC_ALL=C sort -u)
present=$(cd "$ROOT" && find . -type f ! -path ./SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort)
missing=$(LC_ALL=C comm -13 <(printf '%s\n' "$listed") <(printf '%s\n' "$present"))
if [ -n "$missing" ]; then
  echo "STOP: SHA256SUMS does not list $(printf '%s\n' "$missing" | wc -l | tr -d ' ') file(s):"
  printf '%s\n' "$missing" | head -20 | sed 's/^/  /'
  exit 1
fi
echo "SHA256SUMS lists all $(printf '%s\n' "$present" | grep -c .) files"
