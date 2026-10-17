#!/bin/bash
# Read actual payload load commands. Status 3 means a non-Mach-O resource.
set -u
RUNTIME_CHECK="$(cd "$(dirname "$0")" && pwd)/nullmoth-runtime-check"
[ "$#" = 2 ] && [ -x "$RUNTIME_CHECK" ] && [ -d "$1/Library/GPUBundles" ] || { echo "STOP runtime compatibility checker or payload is missing"; exit 1; }
PAYLOAD=$1; OS_VERSION=$2
count=0
while IFS= read -r -d '' binary; do
  output=$("$RUNTIME_CHECK" "$OS_VERSION" "$binary" 2>&1); status=$?
  case "$status" in
    0) printf '%s\n' "$output"; count=$((count+1));;
    3) case "$binary" in *.dylib) echo "STOP a runtime binary has no readable Mach-O load commands"; exit 1;; esac
       case "${binary%/*}" in */Contents/MacOS) echo "STOP a bundle executable has no readable Mach-O load commands"; exit 1;; esac;;
    *) printf '%s\n' "$output"; exit 1;;
  esac
done < <(find "$PAYLOAD/Library/GPUBundles" -type f -print0)
[ "$count" -gt 0 ] || { echo "STOP no runnable x86_64 userland payload was identified"; exit 1; }
