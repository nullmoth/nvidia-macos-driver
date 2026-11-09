#!/bin/bash
# safari_gpu_copy.sh install|remove|status [<bundle>]
#
# Safari's graphics process (com.apple.WebKit.GPU) cannot map /Library/GPUBundles/NVMTLDriver.bundle: its sandbox answers
# "file system sandbox blocked mmap()", so WebGL in Safari is blank and the process logs "Vulkan is not up". The same
# process looks for GPU bundles in the operating-system cryptex first, at
#   /System/Volumes/Preboot/Cryptexes/OS/Library/GPUBundles
# (the dlopen attempts in the logs list it before /Library/GPUBundles). That folder lives on the Preboot volume, not on the
# sealed system volume, so a copy of the driver bundle can be placed there without touching the system volume. This is
# OFF unless the installer is run with NULLMOTH_SAFARI_GPU_COPY=1: whether the sandbox allows mapping from that folder is
# not known without a Safari WebGL run on a Mac with this copy installed.
#
# Never writes under /System/Library or any sealed path: the destination must resolve under /System/Volumes/Preboot, and a
# real write must succeed there (System Integrity Protection protects some of it; then nothing is changed and 0 is returned).
set -u
PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH
ROOT=${NULLMOTH_SAFARI_COPY_ROOT:-/System/Volumes/Preboot}      # a test points this at a temp folder
OSDIR="$ROOT/Cryptexes/OS"
DEST="$OSDIR/Library/GPUBundles"
NAME=NVMTLDriver.bundle
MARK="$DEST/.nullmoth-safari-copy"
cmd=${1:-status}; SRC=${2:-/Library/GPUBundles/$NAME}

under_root() {  # the real path of $1 is inside $ROOT
  local r p; r=$(cd "$ROOT" 2>/dev/null && pwd -P) || return 1
  p=$(cd "$1" 2>/dev/null && pwd -P) || return 1
  case "$p/" in "$r"/*) return 0;; *) return 1;; esac
}

case "$cmd" in
status)
  if [ -f "$MARK" ]; then echo "installed: $DEST/$NAME"; else echo "not installed"; fi ;;
install)
  [ -d "$SRC" ] || { echo "safari copy: $SRC is not there; nothing to copy"; exit 0; }
  [ -d "$OSDIR" ] || { echo "safari copy: $OSDIR does not exist on this Mac; skipped"; exit 0; }
  under_root "$OSDIR" || { echo "safari copy: $OSDIR is not on the Preboot volume; skipped"; exit 0; }
  made=0; [ -d "$DEST" ] || made=1
  if ! mkdir -p "$DEST" 2>/dev/null || ! : > "$DEST/.probe" 2>/dev/null; then
    echo "safari copy: the Preboot volume refuses writes here (System Integrity Protection); nothing changed"
    [ "$made" = 1 ] && rmdir "$DEST" "$OSDIR/Library" 2>/dev/null
    exit 0
  fi
  rm -f "$DEST/.probe"
  rm -rf "${DEST:?}/$NAME"
  if ! ditto "$SRC" "$DEST/$NAME"; then rm -rf "${DEST:?}/$NAME"; echo "safari copy: copy failed; nothing left behind"; exit 1; fi
  chown -R root:wheel "$DEST/$NAME" 2>/dev/null; chmod -R go-w,a+rX "$DEST/$NAME" 2>/dev/null
  { echo "copy of $SRC"; shasum -a 256 "$SRC/Contents/MacOS/NVMTLDriver" 2>/dev/null | cut -d' ' -f1; } > "$MARK"
  echo "safari copy: installed $DEST/$NAME (reboot, then try WebGL in Safari; remove with: sudo $0 remove)" ;;
remove)
  # Only what this script made: the bundle and its marker; the folders go if they are then empty.
  [ -f "$MARK" ] || { echo "safari copy: none installed"; exit 0; }
  rm -rf "${DEST:?}/$NAME"; rm -f "$MARK"
  rmdir "$DEST" "$OSDIR/Library" 2>/dev/null
  echo "safari copy: removed" ;;
*) echo "usage: $0 install|remove|status [<bundle>]"; exit 2 ;;
esac
